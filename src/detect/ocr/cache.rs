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
//!   a boundary) are re-read, not the whole strip. A chat message appearing near
//!   the bottom of the screen re-reads a thin band, not half the display.
//!
//! On a cold scan (or after a resize/reload) there is no baseline, so the work is
//! the whole screen, split into `tiles` overlapping bands for parallelism, one
//! per monitor, so a band never spans the void beside a shorter head in a multi-head
//! bounding box (those undefined pixels destroy recognition of the whole band).

use super::phrase::Word;
use crate::geometry::Rect;

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

/// A rectangular region to OCR, in capture-local coordinates (the top-left of the
/// captured region is the origin). A band never spans more than one monitor, so
/// Tesseract is never handed an image wider or taller than the real content in it.
/// The area beyond a shorter monitor in a multi-head bounding box holds
/// undefined pixels that wreck recognition of everything else in the same image.
#[derive(Debug, Clone, Copy)]
pub(super) struct Band {
    pub x0: i32,
    pub y0: i32,
    pub w: i32,
    pub h: i32,
}

impl Band {
    /// Whether `p`'s position falls inside this band (used to decide which cached
    /// words a re-read replaces).
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x0 && x < self.x0 + self.w && y >= self.y0 && y < self.y0 + self.h
    }
}

/// Incremental OCR cache for one screen size. Not thread-safe on its own.
pub struct ScanCache {
    width: i32,
    height: i32,
    tiles: usize,
    overlap: i32,
    /// The physical monitor rectangles (capture-local). OCR bands are confined to
    /// these so a band never covers a phantom region of the root bounding box.
    /// Empty means "treat the whole capture as one screen" (single-head default).
    monitors: Vec<Rect>,
    /// Whole-screen RGB at the last scan: the baseline bands diff against.
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
            monitors: Vec::new(),
            prev: Vec::new(),
            have_prev: false,
            words: Vec::new(),
        }
    }

    /// Set the physical monitor rectangles (capture-local) that OCR bands must
    /// stay within. Called once by the daemon after it learns the display layout.
    pub fn set_monitors(&mut self, monitors: Vec<Rect>) {
        self.monitors = monitors;
    }

    /// The monitor rectangles to plan bands within, clamped to the capture. Falls
    /// back to the whole capture when none are known, so single-head and tests
    /// behave exactly as before.
    fn regions(&self) -> Vec<Rect> {
        let screen = Rect::new(0, 0, self.width, self.height);
        let clamped: Vec<Rect> = self
            .monitors
            .iter()
            .filter_map(|m| m.clamp_to(screen))
            .collect();
        if clamped.is_empty() {
            vec![screen]
        } else {
            clamped
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
    /// Planning does **not** touch the baseline: each band is adopted only when
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

        let regions = self.regions();
        if self.have_prev {
            // Only split a change band for parallelism when it is genuinely large
            // (a full scroll), not for an ordinary localised edit.
            let chunk = (height / tiles.max(1) as i32).max(CELL as i32 * 4);
            self.changed_bands(rgb, width, height, &regions)
                .into_iter()
                .flat_map(|b| split_band(b, chunk, overlap))
                .collect()
        } else {
            self.words.clear();
            // A cold scan tiles every monitor independently, so no band ever spans
            // the gap between two heads or the void beside a shorter one.
            regions
                .iter()
                .flat_map(|&r| tile_region(r, tiles, overlap))
                .collect()
        }
    }

    /// Replace the words inside a re-read `band` with the freshly OCR'd ones,
    /// keeping every word outside it, and adopt the band's pixels into the
    /// baseline now that it has actually been read. `origin_y` is the capture's
    /// screen-space top (so band coordinates line up with the absolute word
    /// boxes); `rgb` is the capture the band was OCR'd from.
    pub(super) fn splice(
        &mut self,
        rgb: &[u8],
        origin: (i32, i32),
        band: Band,
        new_words: Vec<Word>,
    ) {
        // Adopt the band into the baseline, row segment by row segment (the band
        // is only as wide as its monitor, not the whole capture). Only spliced
        // bands move the baseline, so an aborted scan (whose bands never reach
        // here) re-reads them later.
        let stride = self.width.max(0) as usize * 3;
        let x0 = band.x0.max(0) as usize * 3;
        let xend = ((band.x0 + band.w).max(0) as usize * 3).min(stride);
        if x0 < xend {
            for y in band.y0.max(0)..(band.y0 + band.h).max(0) {
                let base = y as usize * stride;
                let (s, e) = (base + x0, base + xend);
                if e <= self.prev.len() && e <= rgb.len() {
                    self.prev[s..e].copy_from_slice(&rgb[s..e]);
                }
            }
        }
        self.have_prev = true;

        // Replace the cached words that lay inside the band; keep the rest.
        let (ox, oy) = origin;
        self.words.retain(|w| {
            let c = w.rect.center();
            !band.contains(c.x - ox, c.y - oy)
        });
        self.words.extend(new_words);
    }

    /// Every word currently cached.
    pub(super) fn all_words(&self) -> Vec<Word> {
        self.words.clone()
    }

    /// Bands to re-read: within each monitor, the changed cell-rows clustered into
    /// vertical runs (padded by [`BAND_MARGIN`], clipped to the monitor). Cells
    /// outside every monitor are ignored: that void is where a multi-head root
    /// holds the ever-changing undefined pixels that must never trip a re-read.
    /// Empty if only noise (≤ [`NOISE_CELLS`] cells across all monitors) moved.
    fn changed_bands(&self, rgb: &[u8], width: i32, height: i32, regions: &[Rect]) -> Vec<Band> {
        if width <= 0 || height <= 0 {
            return Vec::new();
        }
        let w = width as usize;
        let h = height as usize;
        let cols = w.div_ceil(CELL);
        let cell_rows = h.div_ceil(CELL);
        // Diff against the baseline, but count a differing pixel only when a real
        // monitor covers it. The void beside a shorter head is never baselined (it
        // is never read), so it differs every frame; ignoring it keeps that churn
        // from leaking into a monitor's boundary cell and forcing a phantom re-read.
        let mut changed_px = vec![0u32; cols * cell_rows];
        for y in 0..h {
            let cell_row = (y / CELL) * cols;
            let row = y * w * 3;
            for x in 0..w {
                let i = row + x * 3;
                if (rgb[i] != self.prev[i]
                    || rgb[i + 1] != self.prev[i + 1]
                    || rgb[i + 2] != self.prev[i + 2])
                    && covered(x as i32, y as i32, regions)
                {
                    changed_px[cell_row + x / CELL] += 1;
                }
            }
        }

        let cell_threshold = ((CELL * CELL) as f64 * CELL_CHANGE_FRACTION) as u32;
        let changed = |r: usize, c: usize| changed_px[r * cols + c] > cell_threshold;

        // A cell counts toward the noise gate only if some monitor covers it, so
        // churn in the void beside a shorter head never forces a scan on its own.
        let mut bands = Vec::new();
        let mut total = 0usize;
        for region in regions {
            let (c0, c1) = cell_span(region.x, region.right(), cols);
            let (r0, r1) = cell_span(region.y, region.bottom(), cell_rows);
            let mut row_changed = vec![false; cell_rows];
            for (r, flag) in row_changed.iter_mut().enumerate().take(r1).skip(r0) {
                let n = (c0..c1).filter(|&c| changed(r, c)).count();
                *flag = n > 0;
                total += n;
            }
            // Cluster the monitor's changed cell-rows into vertical runs.
            let mut r = r0;
            while r < r1 {
                if !row_changed[r] {
                    r += 1;
                    continue;
                }
                let start = r;
                while r < r1 && row_changed[r] {
                    r += 1;
                }
                let y0 = ((start * CELL) as i32 - BAND_MARGIN).max(region.y);
                let y1 = ((r * CELL) as i32 + BAND_MARGIN).min(region.bottom());
                bands.push(Band {
                    x0: region.x,
                    y0,
                    w: region.width,
                    h: y1 - y0,
                });
            }
        }
        if total <= NOISE_CELLS {
            return Vec::new();
        }
        bands
    }
}

/// Whether pixel `(x, y)` lies inside any monitor rectangle.
fn covered(x: i32, y: i32, regions: &[Rect]) -> bool {
    regions
        .iter()
        .any(|r| x >= r.x && x < r.right() && y >= r.y && y < r.bottom())
}

/// The half-open cell-index range `[a, b)` of the grid covering pixel range
/// `[lo, hi)`, clamped to `[0, count)`.
fn cell_span(lo: i32, hi: i32, count: usize) -> (usize, usize) {
    let a = (lo.max(0) as usize / CELL).min(count);
    let b = (hi.max(0) as usize).div_ceil(CELL).min(count);
    (a, b.max(a))
}

impl Default for ScanCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Tile one monitor rectangle into `tiles` overlapping horizontal bands (full
/// monitor width), so a cold scan reads each head independently and in parallel.
fn tile_region(region: Rect, tiles: usize, overlap: i32) -> Vec<Band> {
    super::plan_bands(region.height, tiles, overlap)
        .into_iter()
        .map(|(y0, h)| Band {
            x0: region.x,
            y0: region.y + y0,
            w: region.width,
            h,
        })
        .collect()
}

/// Split a band taller than `chunk` into overlapping pieces (in `y` only, keeping
/// its monitor width) so a large change still OCRs in parallel rather than as one
/// giant image.
fn split_band(band: Band, chunk: i32, overlap: i32) -> Vec<Band> {
    let Band { x0, y0, w, h } = band;
    if h <= chunk {
        return vec![band];
    }
    let n = (h + chunk - 1) / chunk;
    let base = (h + n - 1) / n;
    let mut bands = Vec::new();
    for i in 0..n {
        let s = (y0 + i * base - if i > 0 { overlap } else { 0 }).max(y0);
        let e = (y0 + (i + 1) * base + overlap).min(y0 + h);
        if s < e {
            bands.push(Band {
                x0,
                y0: s,
                w,
                h: e - s,
            });
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
            c.splice(
                buf,
                (0, 0),
                b,
                vec![Word::new(Rect::new(0, b.y0, 5, 5), "x")],
            );
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
            c.splice(&buf, (0, 0), b, vec![]);
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
            c.splice(
                &tweaked,
                (0, 0),
                b,
                vec![Word::new(Rect::new(0, 430, 5, 5), "new")],
            );
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
        assert_eq!(
            again.len(),
            1,
            "an un-spliced change is re-planned, not lost"
        );
    }

    #[test]
    fn resize_forces_a_cold_scan() {
        let mut c = ScanCache::new();
        scan(&mut c, &rgb(W, H), W, H);
        assert_eq!(scan(&mut c, &rgb(1200, 600), 1200, 600).len(), TILES);
    }

    /// A tall head on the left beside a shorter one on the right, leaving the
    /// bottom-right of the bounding box a void no monitor covers.
    fn mismatched_heads() -> Vec<Rect> {
        vec![Rect::new(0, 0, 600, 800), Rect::new(600, 0, 400, 500)]
    }

    /// True when a band reaches into the void beside the shorter monitor: the
    /// region (x ≥ 600, y ≥ 500) the bug used to feed to OCR.
    fn enters_void(b: &Band) -> bool {
        b.x0 + b.w > 600 && b.y0 + b.h > 500
    }

    #[test]
    fn cold_scan_tiles_each_monitor_and_skips_the_void() {
        let mut c = ScanCache::new();
        c.set_monitors(mismatched_heads());
        let bands = c.plan(&rgb(1000, 800), 1000, 800, TILES, 20);
        // Every band sits within exactly one head (full head width, inside its
        // height), never the full 1000px bounding-box width.
        for b in &bands {
            let in_left = b.x0 == 0 && b.w == 600 && b.y0 + b.h <= 800;
            let in_right = b.x0 == 600 && b.w == 400 && b.y0 + b.h <= 500;
            assert!(in_left || in_right, "band {b:?} escaped its monitor");
            assert!(!enters_void(b), "band {b:?} reaches into the void");
        }
        // Both heads are actually tiled, not just one.
        assert!(bands.iter().any(|b| b.x0 == 0));
        assert!(bands.iter().any(|b| b.x0 == 600));
    }

    #[test]
    fn a_change_below_a_short_monitor_stays_within_the_tall_one() {
        let mut c = ScanCache::new();
        c.set_monitors(mismatched_heads());
        let buf = rgb(1000, 800);
        let cold = c.plan(&buf, 1000, 800, TILES, 20);
        for &b in &cold {
            c.splice(&buf, (0, 0), b, vec![]);
        }
        // A change at rows 600..640, below the short head, so only the tall head
        // covers it. The re-read band must be the tall head's width, never wider.
        let mut tweaked = buf.clone();
        rewrite_rows(&mut tweaked, 1000, 600, 640);
        let bands = c.plan(&tweaked, 1000, 800, TILES, 20);
        assert!(!bands.is_empty(), "the change is re-read");
        for b in &bands {
            assert_eq!(
                (b.x0, b.w),
                (0, 600),
                "band {b:?} not confined to the tall head"
            );
            assert!(!enters_void(b));
        }
    }

    #[test]
    fn churn_in_the_void_alone_triggers_no_re_read() {
        let mut c = ScanCache::new();
        c.set_monitors(mismatched_heads());
        let buf = rgb(1000, 800);
        let cold = c.plan(&buf, 1000, 800, TILES, 20);
        for &b in &cold {
            c.splice(&buf, (0, 0), b, vec![]);
        }
        // Scribble all over the void (x ≥ 600, y ≥ 500), the undefined region the
        // server fills with noise every frame. No monitor covers it, so a scan
        // must ignore it entirely rather than re-OCR a phantom band.
        let mut tweaked = buf.clone();
        let row = 1000usize * 3;
        for y in 500..800 {
            for x in 600..1000 {
                tweaked[y * row + x * 3] ^= 0xff;
            }
        }
        assert!(
            c.plan(&tweaked, 1000, 800, TILES, 20).is_empty(),
            "void churn must not force a re-read"
        );
    }
}
