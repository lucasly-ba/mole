//! Incremental OCR cache: re-read only the screen strips that changed.
//!
//! The daemon is long-lived and most hints happen on a screen that is largely
//! unchanged since the last one, so re-OCRing everything every time is wasteful.
//! The screen is divided into the same overlapping horizontal strips as the
//! parallel scan ([`super::plan_bands`]); for each strip we remember a hash of
//! its pixels and the words OCR found in it. On the next scan each strip is
//! re-hashed and only the ones whose pixels changed are read again — the rest
//! reuse their cached words. Because adjacent strips overlap, a change near a
//! boundary changes both strips' hashes, so boundary text is never left stale.
//!
//! The cache is held behind a `Mutex` by the detector; the hashing and OCR run
//! without the lock so an on-demand hint and the background pre-warm can share it.

use super::phrase::Word;

/// One horizontal strip's cached state.
struct Strip {
    y0: i32,
    h: i32,
    /// Hash of the strip's pixels at its last OCR, or `None` if never scanned.
    hash: Option<u64>,
    /// Words found in the strip last time it was read (absolute coordinates).
    words: Vec<Word>,
}

/// A strip whose pixels changed and so must be re-read. `Copy` so worker threads
/// can take it by value.
#[derive(Debug, Clone, Copy)]
pub(super) struct DirtyStrip {
    pub index: usize,
    pub y0: i32,
    pub h: i32,
    pub new_hash: u64,
}

/// Per-strip OCR cache for one screen size. Not thread-safe on its own.
pub struct ScanCache {
    width: i32,
    height: i32,
    tiles: usize,
    overlap: i32,
    strips: Vec<Strip>,
}

impl ScanCache {
    pub fn new() -> Self {
        ScanCache {
            width: 0,
            height: 0,
            tiles: 0,
            overlap: 0,
            strips: Vec::new(),
        }
    }

    /// Forget every strip's hash, forcing a full re-scan next time (e.g. after a
    /// config reload that changes OCR parameters).
    pub fn invalidate(&mut self) {
        for s in &mut self.strips {
            s.hash = None;
        }
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
                hash: None,
                words: Vec::new(),
            })
            .collect();
    }

    /// Hash every strip against `rgb` and return the ones whose pixels changed.
    /// Hashes are *not* committed here — the caller OCRs the returned strips and
    /// writes results back with [`ScanCache::store`], so a failed read is retried.
    pub(super) fn diff(
        &mut self,
        rgb: &[u8],
        width: i32,
        height: i32,
        tiles: usize,
        overlap: i32,
    ) -> Vec<DirtyStrip> {
        self.ensure_grid(width, height, tiles, overlap);
        let mut dirty = Vec::new();
        for (index, s) in self.strips.iter().enumerate() {
            let new_hash = hash_band(rgb, width, s.y0, s.h);
            if s.hash != Some(new_hash) {
                dirty.push(DirtyStrip {
                    index,
                    y0: s.y0,
                    h: s.h,
                    new_hash,
                });
            }
        }
        dirty
    }

    /// Commit a freshly OCR'd strip's words and hash.
    pub(super) fn store(&mut self, index: usize, hash: u64, words: Vec<Word>) {
        if let Some(s) = self.strips.get_mut(index) {
            s.words = words;
            s.hash = Some(hash);
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

/// FNV-1a hash of strip rows `[y0, y0 + h)` of a tight, full-width RGB buffer.
/// Cheap relative to OCR (memory-bandwidth bound) and sensitive enough that a
/// single changed character flips the strip's hash.
fn hash_band(rgb: &[u8], width: i32, y0: i32, h: i32) -> u64 {
    let w = width.max(0) as usize;
    let start = (y0.max(0) as usize * w * 3).min(rgb.len());
    let end = ((y0 + h).max(0) as usize * w * 3).min(rgb.len());
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in &rgb[start..end] {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    // A full-screen RGB buffer (width*height*3) filled so each row's bytes encode
    // the row index, making per-strip hashes distinct and change-sensitive.
    fn rgb(width: i32, height: i32, tweak: Option<i32>) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut v = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w * 3 {
                v[y * w * 3 + x] = ((y + x) % 251) as u8;
            }
        }
        if let Some(row) = tweak {
            // Flip one byte on `row` to simulate a localised change.
            v[row as usize * w * 3] ^= 0xff;
        }
        v
    }

    #[test]
    fn first_scan_marks_every_strip_dirty() {
        let mut c = ScanCache::new();
        let buf = rgb(100, 400, None);
        let dirty = c.diff(&buf, 100, 400, 4, 20);
        assert_eq!(
            dirty.len(),
            4,
            "nothing cached yet, so all strips are dirty"
        );
    }

    #[test]
    fn unchanged_screen_is_all_clean_after_store() {
        let mut c = ScanCache::new();
        let buf = rgb(100, 400, None);
        let dirty = c.diff(&buf, 100, 400, 4, 20);
        for d in &dirty {
            c.store(
                d.index,
                d.new_hash,
                vec![Word::new(Rect::new(0, d.y0, 5, 5), "x")],
            );
        }
        // Same pixels -> no strip changed.
        assert!(c.diff(&buf, 100, 400, 4, 20).is_empty());
        assert_eq!(c.all_words().len(), 4, "cached words are kept");
    }

    #[test]
    fn only_the_changed_strip_is_dirty() {
        let mut c = ScanCache::new();
        let buf = rgb(100, 400, None);
        for d in c.diff(&buf, 100, 400, 4, 0) {
            c.store(d.index, d.new_hash, Vec::new());
        }
        // Change a row in the second quarter (y in [100,200)); with no overlap
        // exactly one strip should turn dirty.
        let changed = rgb(100, 400, Some(150));
        let dirty = c.diff(&changed, 100, 400, 4, 0);
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].index, 1);
    }

    #[test]
    fn resizing_rebuilds_the_grid() {
        let mut c = ScanCache::new();
        let _ = c.diff(&rgb(100, 400, None), 100, 400, 4, 20);
        let dirty = c.diff(&rgb(120, 600, None), 120, 600, 4, 20);
        assert_eq!(dirty.len(), 4, "new size starts cold again");
    }
}
