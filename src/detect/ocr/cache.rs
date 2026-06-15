//! Incremental OCR cache: re-read only the regions of the screen that changed.
//!
//! The daemon is long-lived and most hints land on a screen that barely changed
//! since the last one, so re-OCRing everything every time is wasteful. The cache
//! keeps the whole-screen pixels from the last scan (the baseline) plus the flat
//! list of words found. On the next scan it diffs against the baseline at the
//! granularity of small **cells**, clusters the changed cells' rows into tight
//! horizontal **bands**, and asks the detector to re-OCR only those bands. The
//! freshly read words are spliced in: words inside a re-read band are replaced,
//! everything outside is kept. A hint on a static screen reads nothing.
//!
//! Two deliberate choices keep it stable on a real desktop:
//!
//! * **Locality, not an exact hash.** A cell counts as changed only when a real
//!   fraction of its pixels differ, and the screen is only re-read when more than
//!   a couple of cells changed. A ticking clock or a blinking text caret is one
//!   or two cells and is ignored; a scroll or a typed-out line is many and is
//!   caught. (An exact hash made every flicker re-OCR a whole region.)
//! * **Tight bands.** Only the changed rows (plus a margin for text that straddles
//!   a boundary) are re-read, not the whole strip — a chat message appearing near
//!   the bottom of the screen re-reads a thin band, not half the display.
//!
//! On a cold scan (or after a resize/reload) there is no baseline, so the work is
//! the whole screen, split into `tiles` overlapping bands for parallelism.

use super::phrase::Word;

/// Side length (px) of a change-detection cell.
const CELL: usize = 96;
/// A cell counts as changed when more than this fraction of its pixels differ.
/// High enough to shrug off a caret or sub-pixel antialiasing within the cell.
const CELL_CHANGE_FRACTION: f64 = 0.05;
/// The screen is re-read only when more than this many cells changed. Absorbs a
/// couple of independent noise sources (a clock *and* a caret).
const NOISE_CELLS: usize = 2;
/// Pixels added above and below a changed run, so a line of text that straddles
/// the edge of the changed area is fully inside the re-read band.
const BAND_MARGIN: i32 = 64;

/// A full-width horizontal band to OCR, in capture-local coordinates (the top of
/// the captured region is `y = 0`).
#[derive(Debug, Clone, Copy)]
pub(super) struct Band {
    pub y0: i32,
    pub h: i32,
}

/// Incremental OCR cache for one screen size. Not thread-safe on its own.
pub struct ScanCache {
    width: i32,
    height: i32,
    tiles: usize,
    overlap: i32,
    /// Whole-screen RGB at the last scan — the baseline bands diff against.
    prev: Vec<u8>,
    have_prev: bool,
    /// All words currently believed on screen, in absolute coordinates.
    words: Vec<Word>,
}

impl ScanCache {
    pub fn new() -> Self {
        ScanCache {
            width: 0,
            height: 0,
            tiles: 0,
            overlap: 0,
            prev: Vec::new(),
            have_prev: false,
            words: Vec::new(),
        }
    }

    /// Drop the baseline, forcing a full re-scan next time (e.g. after a config
    /// reload that changes OCR parameters).
    pub fn invalidate(&mut self) {
        self.have_prev = false;
    }

    fn ensure_grid(&mut self, width: i32, height: i32, tiles: usize, overlap: i32) {
        if self.width == width
            && self.height == height
            && self.tiles == tiles
            && self.overlap == overlap
        {
            return;
        }
        self.width = width;
        self.height = height;
        self.tiles = tiles;
        self.overlap = overlap;
        self.have_prev = false;
        self.words.clear();
    }

    /// Decide which bands to OCR for the capture `rgb`. Cold scans return the
    /// whole screen as `tiles` overlapping bands; warm scans return tight bands
    /// around what changed, or nothing if only noise moved.
    ///
    /// Planning does **not** touch the baseline — each band is adopted only when
    /// it is actually [`splice`](ScanCache::splice)d back in. So a scan that is
    /// aborted before its bands are read leaves the baseline (and the cached
    /// words) untouched, and those regions are simply re-planned next time rather
    /// than going stale.
    pub(super) fn plan(
        &mut self,
        rgb: &[u8],
        width: i32,
        height: i32,
        tiles: usize,
        overlap: i32,
    ) -> Vec<Band> {
        self.ensure_grid(width, height, tiles, overlap);
        if self.prev.len() != rgb.len() {
            self.prev = vec![0u8; rgb.len()];
            self.have_prev = false;
            self.words.clear();
        }

        if self.have_prev {
            // Only split a change band for parallelism when it is genuinely large
            // (a full scroll), not for an ordinary localised edit.
            let chunk = (height / tiles.max(1) as i32).max(CELL as i32 * 4);
            self.changed_runs(rgb, width, height)
                .into_iter()
                .flat_map(|(y0, h)| split_band(y0, h, chunk, overlap))
                .collect()
        } else {
            self.words.clear();
            super::plan_bands(height, tiles, overlap)
                .into_iter()
                .map(|(y0, h)| Band { y0, h })
                .collect()
        }
    }

    /// Replace the words inside a re-read `band` with the freshly OCR'd ones,
    /// keeping every word outside it, and adopt the band's pixels into the
    /// baseline now that it has actually been read. `origin_y` is the capture's
    /// screen-space top (so band coordinates line up with the absolute word
    /// boxes); `rgb` is the capture the band was OCR'd from.
    pub(super) fn splice(&mut self, rgb: &[u8], origin_y: i32, band: Band, new_words: Vec<Word>) {
        // Adopt the band into the baseline. Only spliced bands move the baseline,
        // so an aborted scan (whose bands never reach here) re-reads them later.
        let row = self.width.max(0) as usize * 3;
        let start = (band.y0.max(0) as usize * row)
            .min(self.prev.len())
            .min(rgb.len());
        let end = ((band.y0 + band.h).max(0) as usize * row)
            .min(self.prev.len())
            .min(rgb.len());
        if start < end {
            self.prev[start..end].copy_from_slice(&rgb[start..end]);
        }
        self.have_prev = true;

        let y0 = origin_y + band.y0;
        let y1 = y0 + band.h;
        self.words
            .retain(|w| w.rect.center().y < y0 || w.rect.center().y >= y1);
        self.words.extend(new_words);
    }

    /// Every word currently cached.
    pub(super) fn all_words(&self) -> Vec<Word> {
        self.words.clone()
    }

    /// Changed cell-rows clustered into `(y0, height)` runs (capture-local),
    /// padded by [`BAND_MARGIN`]. Empty if only noise (≤ [`NOISE_CELLS`]) moved.
    fn changed_runs(&self, rgb: &[u8], width: i32, height: i32) -> Vec<(i32, i32)> {
        if width <= 0 || height <= 0 {
            return Vec::new();
        }
        let w = width as usize;
        let h = height as usize;
        let cols = w.div_ceil(CELL);
        let cell_rows = h.div_ceil(CELL);
        let mut changed_px = vec![0u32; cols * cell_rows];
        for y in 0..h {
            let cell_row = (y / CELL) * cols;
            let row = y * w * 3;
            for x in 0..w {
                let i = row + x * 3;
                if rgb[i] != self.prev[i]
                    || rgb[i + 1] != self.prev[i + 1]
                    || rgb[i + 2] != self.prev[i + 2]
                {
                    changed_px[cell_row + x / CELL] += 1;
                }
            }
        }

        let cell_threshold = ((CELL * CELL) as f64 * CELL_CHANGE_FRACTION) as u32;
        let mut row_changed = vec![false; cell_rows];
        let mut total = 0usize;
        for r in 0..cell_rows {
            for c in 0..cols {
                if changed_px[r * cols + c] > cell_threshold {
                    row_changed[r] = true;
                    total += 1;
                }
            }
        }
        if total <= NOISE_CELLS {
            return Vec::new();
        }

        let mut runs = Vec::new();
        let mut r = 0;
        while r < cell_rows {
            if !row_changed[r] {
                r += 1;
                continue;
            }
            let start = r;
            while r < cell_rows && row_changed[r] {
                r += 1;
            }
            let y0 = ((start * CELL) as i32 - BAND_MARGIN).max(0);
            let y1 = ((r * CELL) as i32 + BAND_MARGIN).min(height);
            runs.push((y0, y1 - y0));
        }
        runs
    }
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Split a band taller than `chunk` into overlapping pieces so a large change
/// still OCRs in parallel rather than as one giant image.
fn split_band(y0: i32, h: i32, chunk: i32, overlap: i32) -> Vec<Band> {
    if h <= chunk {
        return vec![Band { y0, h }];
    }
    let n = (h + chunk - 1) / chunk;
    let base = (h + n - 1) / n;
    let mut bands = Vec::new();
    for i in 0..n {
        let s = (y0 + i * base - if i > 0 { overlap } else { 0 }).max(y0);
        let e = (y0 + (i + 1) * base + overlap).min(y0 + h);
        if s < e {
            bands.push(Band { y0: s, h: e - s });
        }
        if y0 + (i + 1) * base >= y0 + h {
            break;
        }
    }
    bands
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    const W: i32 = 1000;
    const H: i32 = 800;
    const TILES: usize = 4;

    fn rgb(width: i32, height: i32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut v = vec![0u8; w * h * 3];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        v
    }

    /// Run a scan: plan the bands and fake an OCR result for each (one word per
    /// band, tagged with its y), returning the bands planned.
    fn scan(c: &mut ScanCache, buf: &[u8], w: i32, h: i32) -> Vec<Band> {
        let bands = c.plan(buf, w, h, TILES, 20);
        for &b in &bands {
            c.splice(buf, 0, b, vec![Word::new(Rect::new(0, b.y0, 5, 5), "x")]);
        }
        bands
    }

    /// Rewrite rows `[y0, y1)` of `buf` so they read as genuinely changed.
    fn rewrite_rows(buf: &mut [u8], w: i32, y0: usize, y1: usize) {
        let row = w as usize * 3;
        for b in &mut buf[y0 * row..y1 * row] {
            *b = b.wrapping_add(97);
        }
    }

    #[test]
    fn cold_scan_covers_the_whole_screen_in_tiles() {
        let mut c = ScanCache::new();
        assert_eq!(scan(&mut c, &rgb(W, H), W, H).len(), TILES);
    }

    #[test]
    fn unchanged_screen_reads_nothing() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H);
        assert!(scan(&mut c, &buf, W, H).is_empty());
    }

    #[test]
    fn tiny_localised_change_is_ignored() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H);
        let mut tweaked = buf.clone();
        let row = W as usize * 3;
        for dx in 0..3 {
            tweaked[300 * row + dx * 3] ^= 0xff; // a "caret"
        }
        for y in 600..630 {
            for x in 500..520 {
                tweaked[y * row + x * 3] ^= 0xff; // a "clock digit"
            }
        }
        assert!(scan(&mut c, &tweaked, W, H).is_empty());
    }

    #[test]
    fn a_real_change_reads_only_a_tight_band() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H);
        // Rewrite rows 400..480 (a chat message appearing).
        let mut tweaked = buf.clone();
        rewrite_rows(&mut tweaked, W, 400, 480);
        let bands = c.plan(&tweaked, W, H, TILES, 20);
        assert_eq!(bands.len(), 1, "one contiguous change -> one band");
        // The band is tight: it covers the change plus margin, not the whole screen.
        assert!(bands[0].y0 <= 400 && bands[0].y0 >= 400 - CELL as i32 - BAND_MARGIN);
        assert!(bands[0].h < H / 2, "band much smaller than the screen");
    }

    #[test]
    fn two_far_apart_changes_are_two_bands() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H);
        let mut tweaked = buf.clone();
        rewrite_rows(&mut tweaked, W, 100, 140);
        rewrite_rows(&mut tweaked, W, 600, 640);
        let bands = c.plan(&tweaked, W, H, TILES, 20);
        assert_eq!(bands.len(), 2);
    }

    #[test]
    fn splice_keeps_words_outside_the_band() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        // Cold scan to establish a baseline (only splicing adopts it now).
        let cold = c.plan(&buf, W, H, TILES, 20);
        for &b in &cold {
            c.splice(&buf, 0, b, vec![]);
        }
        // Seed with words at known rows across the screen.
        c.words = vec![
            Word::new(Rect::new(0, 50, 5, 5), "top"),
            Word::new(Rect::new(0, 430, 5, 5), "middle"),
            Word::new(Rect::new(0, 700, 5, 5), "bottom"),
        ];
        // A change at rows 400..460 replaces only "middle".
        let mut tweaked = buf.clone();
        rewrite_rows(&mut tweaked, W, 400, 460);
        let bands = c.plan(&tweaked, W, H, TILES, 20);
        for &b in &bands {
            c.splice(&tweaked, 0, b, vec![Word::new(Rect::new(0, 430, 5, 5), "new")]);
        }
        let words = c.all_words();
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert!(texts.contains(&"top") && texts.contains(&"bottom"));
        assert!(texts.contains(&"new") && !texts.contains(&"middle"));
    }

    #[test]
    fn an_unspliced_scan_does_not_adopt_the_baseline() {
        // A scan that plans bands but is aborted before any splice must leave the
        // baseline untouched, so the same change is re-planned next time rather
        // than silently treated as already-read.
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H); // establish a baseline

        let mut tweaked = buf.clone();
        rewrite_rows(&mut tweaked, W, 400, 480);
        let first = c.plan(&tweaked, W, H, TILES, 20);
        assert_eq!(first.len(), 1, "the change is planned once");
        // Abort: no splice happens. The next plan over the same pixels must still
        // see the change (baseline was not moved).
        let again = c.plan(&tweaked, W, H, TILES, 20);
        assert_eq!(again.len(), 1, "an un-spliced change is re-planned, not lost");
    }

    #[test]
    fn resize_forces_a_cold_scan() {
        let mut c = ScanCache::new();
        scan(&mut c, &rgb(W, H), W, H);
        assert_eq!(scan(&mut c, &rgb(1200, 600), 1200, 600).len(), TILES);
    }
}
