//! Incremental OCR cache: re-read only the screen strips that meaningfully changed.
//!
//! The daemon is long-lived and most hints happen on a screen that is largely
//! unchanged since the last one, so re-OCRing everything every time is wasteful.
//! The screen is divided into the same overlapping horizontal strips as the
//! parallel scan ([`super::plan_bands`]); for each strip we keep the pixels it
//! had at its last OCR plus the words found in it. On the next scan each strip is
//! compared to those pixels and only the ones that changed by more than a small
//! fraction are read again — the rest reuse their cached words.
//!
//! Comparing by *locality* rather than an exact hash is deliberate. Each strip
//! is divided into a grid of small cells; a cell only counts as changed when a
//! real fraction of its pixels differ (so antialiasing and a blinking caret —
//! a few pixels — don't register at all), and a strip is only re-read when more
//! than a couple of cells changed. A ticking clock or a blinking text caret
//! touches one or two cells and is ignored; a scroll, a new window or a typed-out
//! line touches many and is caught. An exact hash, by contrast, made every such
//! tiny flicker re-OCR a whole dense strip, so the cache never settled.
//!
//! Because adjacent strips overlap, a change near a boundary registers in both
//! strips, so boundary text is never left stale. The cache is held behind a
//! `Mutex` by the detector.

use super::phrase::Word;

/// Side length (px) of a change-detection cell.
const CELL: usize = 96;
/// A cell counts as changed when more than this fraction of its pixels differ.
/// High enough to shrug off a caret or sub-pixel antialiasing within the cell.
const CELL_CHANGE_FRACTION: f64 = 0.05;
/// A strip is re-read only when more than this many cells changed. Absorbs a
/// couple of independent noise sources (a clock *and* a caret) in one strip.
const NOISE_CELLS: usize = 2;

/// One horizontal strip's cached state.
struct Strip {
    y0: i32,
    h: i32,
    /// Whether `words`/baseline pixels have been populated by an OCR pass.
    scanned: bool,
    /// Words found in the strip last time it was read (absolute coordinates).
    words: Vec<Word>,
}

/// A strip that changed enough to be re-read. `Copy` so worker threads can take
/// it by value.
#[derive(Debug, Clone, Copy)]
pub(super) struct DirtyStrip {
    pub index: usize,
    pub y0: i32,
    pub h: i32,
}

/// Per-strip OCR cache for one screen size. Not thread-safe on its own.
pub struct ScanCache {
    width: i32,
    height: i32,
    tiles: usize,
    overlap: i32,
    strips: Vec<Strip>,
    /// The whole-screen RGB at the last scan, the baseline strips diff against.
    prev: Vec<u8>,
    have_prev: bool,
}

impl ScanCache {
    pub fn new() -> Self {
        ScanCache {
            width: 0,
            height: 0,
            tiles: 0,
            overlap: 0,
            strips: Vec::new(),
            prev: Vec::new(),
            have_prev: false,
        }
    }

    /// Forget the baseline, forcing a full re-scan next time (e.g. after a config
    /// reload that changes OCR parameters).
    pub fn invalidate(&mut self) {
        self.have_prev = false;
    }

    /// (Re)build the strip grid if the screen size, tile count or overlap changed.
    fn ensure_grid(&mut self, width: i32, height: i32, tiles: usize, overlap: i32) {
        if self.width == width
            && self.height == height
            && self.tiles == tiles
            && self.overlap == overlap
            && !self.strips.is_empty()
        {
            return;
        }
        self.width = width;
        self.height = height;
        self.tiles = tiles;
        self.overlap = overlap;
        self.strips = super::plan_bands(height, tiles, overlap)
            .into_iter()
            .map(|(y0, h)| Strip {
                y0,
                h,
                scanned: false,
                words: Vec::new(),
            })
            .collect();
        self.have_prev = false;
    }

    /// Compare every strip against the baseline and return those that changed by
    /// more than [`CHANGE_FRACTION`]. The returned strips' pixels are folded into
    /// the baseline immediately (they're about to be re-OCR'd), so noise that
    /// stays below the threshold accumulates and is eventually caught.
    pub(super) fn diff(
        &mut self,
        rgb: &[u8],
        width: i32,
        height: i32,
        tiles: usize,
        overlap: i32,
    ) -> Vec<DirtyStrip> {
        self.ensure_grid(width, height, tiles, overlap);
        if self.prev.len() != rgb.len() {
            self.prev = vec![0u8; rgb.len()];
            self.have_prev = false;
        }

        let w = width.max(0) as usize;
        let mut dirty = Vec::new();
        for (index, s) in self.strips.iter().enumerate() {
            let start = (s.y0.max(0) as usize * w * 3).min(rgb.len());
            let end = ((s.y0 + s.h).max(0) as usize * w * 3).min(rgb.len());
            let changed = !self.have_prev
                || !s.scanned
                || strip_changed(&rgb[start..end], &self.prev[start..end], w);
            if changed {
                dirty.push(DirtyStrip {
                    index,
                    y0: s.y0,
                    h: s.h,
                });
                self.prev[start..end].copy_from_slice(&rgb[start..end]);
            }
        }
        self.have_prev = true;
        dirty
    }

    /// Commit a freshly OCR'd strip's words.
    pub(super) fn store(&mut self, index: usize, words: Vec<Word>) {
        if let Some(s) = self.strips.get_mut(index) {
            s.words = words;
            s.scanned = true;
        }
    }

    /// All cached words across every strip (clean and just-updated alike).
    pub(super) fn all_words(&self) -> Vec<Word> {
        let mut out = Vec::new();
        for s in &self.strips {
            out.extend(s.words.iter().cloned());
        }
        out
    }
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
}

/// True when more than [`NOISE_CELLS`] cells of the strip changed, where a cell
/// changed means more than [`CELL_CHANGE_FRACTION`] of its pixels differ. `width`
/// is the strip's pixel width (its height is inferred from the slice length).
fn strip_changed(now: &[u8], base: &[u8], width: usize) -> bool {
    if width == 0 {
        return now != base;
    }
    let row_bytes = width * 3;
    let height = now.len() / row_bytes;
    let cols = width.div_ceil(CELL);
    let rows = height.div_ceil(CELL);
    let mut changed_px = vec![0u32; cols * rows];
    for y in 0..height {
        let cell_row = (y / CELL) * cols;
        let row = y * row_bytes;
        for x in 0..width {
            let i = row + x * 3;
            if now[i] != base[i] || now[i + 1] != base[i + 1] || now[i + 2] != base[i + 2] {
                changed_px[cell_row + x / CELL] += 1;
            }
        }
    }
    let cell_threshold = ((CELL * CELL) as f64 * CELL_CHANGE_FRACTION) as u32;
    let changed_cells = changed_px.iter().filter(|&&c| c > cell_threshold).count();
    changed_cells > NOISE_CELLS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn rgb(width: i32, height: i32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut v = vec![0u8; w * h * 3];
        for (i, b) in v.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        v
    }

    fn scan(c: &mut ScanCache, buf: &[u8], w: i32, h: i32, tiles: usize, ov: i32) -> usize {
        let dirty = c.diff(buf, w, h, tiles, ov);
        for d in &dirty {
            c.store(d.index, vec![Word::new(Rect::new(0, d.y0, 5, 5), "x")]);
        }
        dirty.len()
    }

    const W: i32 = 1000;
    const H: i32 = 400;

    #[test]
    fn first_scan_marks_every_strip_dirty() {
        let mut c = ScanCache::new();
        assert_eq!(scan(&mut c, &rgb(W, H), W, H, 4, 20), 4);
    }

    #[test]
    fn unchanged_screen_is_all_clean() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H, 4, 20);
        assert_eq!(scan(&mut c, &buf, W, H, 4, 20), 0, "nothing changed");
        assert_eq!(c.all_words().len(), 4, "cached words are kept");
    }

    #[test]
    fn tiny_localised_change_is_ignored() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H, 4, 0);
        // Flip a caret-sized blob and a clock-sized blob in strip 1 (rows
        // 100..200): two small cells, which must read as noise.
        let mut tweaked = buf.clone();
        let row_bytes = W as usize * 3;
        for dx in 0..3 {
            // ~3px "caret" near the left
            tweaked[110 * row_bytes + dx * 3] ^= 0xff;
        }
        for y in 120..150 {
            for x in 500..520 {
                // ~20x30px "clock digit" mid-strip
                tweaked[y * row_bytes + x * 3] ^= 0xff;
            }
        }
        assert_eq!(
            scan(&mut c, &tweaked, W, H, 4, 0),
            0,
            "a caret and a clock digit must not dirty the strip"
        );
    }

    #[test]
    fn real_change_dirties_only_its_strip() {
        let mut c = ScanCache::new();
        let buf = rgb(W, H);
        scan(&mut c, &buf, W, H, 4, 0);
        // Rewrite all of strip 1's rows (a scroll / new content).
        let mut tweaked = buf.clone();
        let row_bytes = W as usize * 3;
        for b in &mut tweaked[100 * row_bytes..200 * row_bytes] {
            *b = b.wrapping_add(123);
        }
        let dirty = c.diff(&tweaked, W, H, 4, 0);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].index, 1);
    }

    #[test]
    fn resizing_rebuilds_the_grid() {
        let mut c = ScanCache::new();
        scan(&mut c, &rgb(W, H), W, H, 4, 20);
        assert_eq!(
            scan(&mut c, &rgb(1200, 600), 1200, 600, 4, 20),
            4,
            "new size starts cold"
        );
    }
}
