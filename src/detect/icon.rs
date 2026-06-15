//! Icon / button detection: hint things you can click that have no text.
//!
//! OCR finds text, but a lot of clickable UI is icon-only — toolbar buttons,
//! tab favicons, the window controls, a hamburger menu. Those carry no glyphs
//! for Tesseract to read, so they'd be unreachable with text hints alone.
//!
//! This finds them cheaply from the pixels, no extra processes: lay a fine grid
//! over the screen, mark each cell that holds an *edge* (a real spread between
//! its lightest and darkest pixel — flat background is ignored), cluster the
//! marked cells into connected blobs, and keep the blobs that look like an icon
//! — roughly icon-sized, compact, and not sitting on text we already hinted. It
//! is heuristic and deliberately conservative: a handful of stray hints on a
//! busy image is a fair price for being able to click the toolbar.

use crate::detect::Element;
use crate::geometry::Rect;

/// Side length (px) of a detection cell. Small enough to resolve a ~16px icon,
/// big enough that the whole-screen pass stays cheap.
const CELL: i32 = 8;
/// Minimum spread between the lightest and darkest pixel in a cell for it to
/// count as holding an edge (0..=765 over summed RGB). Shrugs off gradients and
/// anti-aliasing on a flat fill.
const EDGE_SPREAD: i32 = 90;
/// Icon bounding box must be at least this wide/tall (px) — below it is a single
/// glyph or speck, not a control.
const MIN_ICON: i32 = 16;
/// …and at most this wide/tall (px) — above it is an image or a text block.
const MAX_ICON: i32 = 80;
/// Reject very elongated blobs (a rule, a separator, a missed line of text):
/// the longer side may be at most this multiple of the shorter.
const MAX_ASPECT: f64 = 3.2;
/// A blob must fill at least this fraction of its bounding box to be a solid
/// control rather than a sparse scattering of edges.
const MIN_FILL: f64 = 0.30;
/// An icon sits in padding, so the ring of cells just outside its box should be
/// mostly quiet. Reject candidates whose surrounding ring is busier than this —
/// that's a fragment embedded in text or dense UI, not a standalone control.
const MAX_RING_BUSY: f64 = 0.22;
/// Skip a candidate that overlaps an already-hinted text box by more than this
/// fraction of the candidate's area — it's text, not an icon.
const MAX_TEXT_OVERLAP: f64 = 0.35;
/// Hard cap on icon hints, so a pathological screen can't bury everything.
const MAX_ICONS: usize = 200;

/// Find icon-like clickable regions in `rgb` (tight RGB, `width`×`height`), in
/// absolute coordinates offset by `region`'s origin, excluding anything that
/// overlaps the already-detected `text` boxes.
pub fn detect(
    rgb: &[u8],
    width: i32,
    height: i32,
    region: Rect,
    text: &[Element],
    min_size: i32,
) -> Vec<Element> {
    if width <= 0 || height <= 0 {
        return Vec::new();
    }
    let cols = (width / CELL).max(1);
    let rows = (height / CELL).max(1);
    let edges = edge_cells(rgb, width, height, cols, rows);

    let mut icons = Vec::new();
    for bbox in connected_blobs(&edges, cols, rows) {
        // Cell-space bbox -> absolute pixel rect.
        let rect = Rect::new(
            region.x + bbox.x0 * CELL,
            region.y + bbox.y0 * CELL,
            (bbox.x1 - bbox.x0 + 1) * CELL,
            (bbox.y1 - bbox.y0 + 1) * CELL,
        );
        if !icon_shaped(rect, min_size) {
            continue;
        }
        if bbox.fill() < MIN_FILL {
            continue;
        }
        if ring_busy_fraction(&edges, cols, rows, &bbox) > MAX_RING_BUSY {
            continue; // embedded in text / dense UI, not a standalone icon
        }
        if overlaps_text(rect, text) {
            continue;
        }
        icons.push(Element::new(rect, ""));
        if icons.len() >= MAX_ICONS {
            break;
        }
    }
    icons
}

/// Mark every grid cell that holds an edge — a spread between its lightest and
/// darkest summed-RGB pixel above [`EDGE_SPREAD`].
fn edge_cells(rgb: &[u8], width: i32, height: i32, cols: i32, rows: i32) -> Vec<bool> {
    let w = width as usize;
    let mut cells = vec![false; (cols * rows) as usize];
    for cy in 0..rows {
        for cx in 0..cols {
            let x0 = (cx * CELL) as usize;
            let y0 = (cy * CELL) as usize;
            let x1 = ((cx + 1) * CELL).min(width) as usize;
            let y1 = ((cy + 1) * CELL).min(height) as usize;
            // Sample every other pixel on both axes: a ~16px icon still spans
            // several samples, and it quarters the whole-screen pass.
            let (mut lo, mut hi) = (i32::MAX, i32::MIN);
            for y in (y0..y1).step_by(2) {
                let row = y * w * 3;
                for x in (x0..x1).step_by(2) {
                    let i = row + x * 3;
                    if i + 2 < rgb.len() {
                        let lum = rgb[i] as i32 + rgb[i + 1] as i32 + rgb[i + 2] as i32;
                        lo = lo.min(lum);
                        hi = hi.max(lum);
                    }
                }
            }
            if hi - lo >= EDGE_SPREAD {
                cells[(cy * cols + cx) as usize] = true;
            }
        }
    }
    cells
}

/// A blob's bounding box in cell coordinates, plus the cell count it actually
/// covers (for the fill ratio).
struct Blob {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    cells: i32,
}

impl Blob {
    fn fill(&self) -> f64 {
        let area = (self.x1 - self.x0 + 1) * (self.y1 - self.y0 + 1);
        if area <= 0 {
            0.0
        } else {
            self.cells as f64 / area as f64
        }
    }
}

/// Flood-fill the edge grid into 4-connected blobs, returning each one's box.
fn connected_blobs(edges: &[bool], cols: i32, rows: i32) -> Vec<Blob> {
    let mut seen = vec![false; edges.len()];
    let mut blobs = Vec::new();
    let mut stack = Vec::new();
    for start in 0..edges.len() {
        if !edges[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        let mut count = 0;
        while let Some(idx) = stack.pop() {
            let cx = idx as i32 % cols;
            let cy = idx as i32 / cols;
            x0 = x0.min(cx);
            y0 = y0.min(cy);
            x1 = x1.max(cx);
            y1 = y1.max(cy);
            count += 1;
            for (nx, ny) in [(cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)] {
                if nx < 0 || ny < 0 || nx >= cols || ny >= rows {
                    continue;
                }
                let n = (ny * cols + nx) as usize;
                if edges[n] && !seen[n] {
                    seen[n] = true;
                    stack.push(n);
                }
            }
        }
        blobs.push(Blob {
            x0,
            y0,
            x1,
            y1,
            cells: count,
        });
    }
    blobs
}

/// Fraction of the one-cell ring just outside `bbox` that holds an edge. Low for
/// a control surrounded by padding; high for a fragment buried in text.
fn ring_busy_fraction(edges: &[bool], cols: i32, rows: i32, bbox: &Blob) -> f64 {
    let (mut busy, mut total) = (0, 0);
    for cy in (bbox.y0 - 1)..=(bbox.y1 + 1) {
        for cx in (bbox.x0 - 1)..=(bbox.x1 + 1) {
            // Only the cells on the ring, not the interior.
            let on_ring = cx < bbox.x0 || cx > bbox.x1 || cy < bbox.y0 || cy > bbox.y1;
            if !on_ring || cx < 0 || cy < 0 || cx >= cols || cy >= rows {
                continue;
            }
            total += 1;
            if edges[(cy * cols + cx) as usize] {
                busy += 1;
            }
        }
    }
    if total == 0 {
        0.0
    } else {
        busy as f64 / total as f64
    }
}

/// Whether a pixel rect is plausibly an icon: within the size band, not too
/// elongated, and at least `min_size` on each side.
fn icon_shaped(rect: Rect, min_size: i32) -> bool {
    let (w, h) = (rect.width, rect.height);
    if w < MIN_ICON.max(min_size) || h < MIN_ICON.max(min_size) {
        return false;
    }
    if w > MAX_ICON || h > MAX_ICON {
        return false;
    }
    let (long, short) = if w >= h { (w, h) } else { (h, w) };
    short > 0 && (long as f64 / short as f64) <= MAX_ASPECT
}

/// Whether `rect` sits mostly on top of an already-hinted text box.
fn overlaps_text(rect: Rect, text: &[Element]) -> bool {
    let area = (rect.width * rect.height).max(1);
    text.iter().any(|e| {
        rect.clamp_to(e.rect).map_or(0, |i| i.area()) as f64 / area as f64 > MAX_TEXT_OVERLAP
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paint a filled rectangle of bright pixels into a black RGB buffer.
    fn fill(rgb: &mut [u8], width: i32, r: Rect) {
        let w = width as usize;
        for y in r.y..r.bottom() {
            for x in r.x..r.right() {
                let i = (y as usize * w + x as usize) * 3;
                if i + 2 < rgb.len() {
                    rgb[i] = 255;
                    rgb[i + 1] = 255;
                    rgb[i + 2] = 255;
                }
            }
        }
    }

    #[test]
    fn finds_an_isolated_icon_sized_blob() {
        let (w, h) = (400, 300);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        fill(&mut rgb, w, Rect::new(100, 100, 24, 24)); // a 24px "icon"
        let icons = detect(&rgb, w, h, Rect::new(0, 0, w, h), &[], 6);
        assert_eq!(icons.len(), 1);
        let r = icons[0].rect;
        assert!(r.x <= 100 && r.right() >= 124, "box covers the icon");
    }

    #[test]
    fn ignores_a_huge_image_block() {
        let (w, h) = (400, 300);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        fill(&mut rgb, w, Rect::new(20, 20, 200, 200)); // way bigger than an icon
        assert!(detect(&rgb, w, h, Rect::new(0, 0, w, h), &[], 6).is_empty());
    }

    #[test]
    fn skips_a_blob_sitting_on_hinted_text() {
        let (w, h) = (400, 300);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        fill(&mut rgb, w, Rect::new(100, 100, 24, 24));
        let text = [Element::new(Rect::new(96, 96, 32, 32), "x")];
        assert!(detect(&rgb, w, h, Rect::new(0, 0, w, h), &text, 6).is_empty());
    }

    #[test]
    fn ignores_a_flat_background() {
        let (w, h) = (200, 200);
        let rgb = vec![30u8; (w * h * 3) as usize]; // uniform — no edges
        assert!(detect(&rgb, w, h, Rect::new(0, 0, w, h), &[], 6).is_empty());
    }
}
