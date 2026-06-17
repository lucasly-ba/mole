//! Hint box placement with anti-overlap (Plan §2.2).
//!
//! Each detected element gets a small label box anchored at its top-left corner.
//! When boxes would collide (dense UIs, lists), [`place_hints`] nudges later
//! boxes to nearby free positions so labels stay readable. The point the cursor
//! ultimately jumps to is the *start* of the element's text (its left edge,
//! vertically centred), not the phrase centre: a phrase target spans a whole
//! line, and its midpoint can land in a gap between words or past the clickable
//! part. The first glyphs are where you actually want to click.
//!
//! Drag selection ([`place_drag_hints`]) is different: each phrase gets *two*
//! hints, one on the first glyph and one just past the last, so you can pick the
//! start of one phrase and the end of another and drag-select everything between:
//! a whole sentence, copied to the clipboard (Plan §4.2).

use crate::detect::Element;
use crate::geometry::{Point, Rect};

/// A label box ready to be rendered.
#[derive(Debug, Clone)]
pub struct HintBox {
    /// Index into the original element list (and into the label list).
    pub index: usize,
    pub label: String,
    /// Where the label box is drawn.
    pub rect: Rect,
    /// Where the pointer should land when this hint is chosen.
    pub target: Point,
}

/// The point to jump to for an element: the start of its text, the left edge,
/// vertically centred, nudged in by about half a line height so it lands on the
/// first glyph rather than the very edge (or a gap before it).
fn text_start(rect: Rect) -> Point {
    let inset = (rect.height / 2).min(rect.width / 2);
    Point::new(rect.x + inset, rect.y + rect.height / 2)
}

/// Horizontal margin (px) by which a drag press/release overshoots the text box,
/// scaled to the line height (≈ half a character) and clamped. The OCR box hugs
/// the ink, so a press *on* the left edge lands inside the first glyph and the
/// selection starts at the second character; nudging out by this margin puts the
/// press in the gap just before the first glyph (and the release just past the
/// last), so the whole phrase (first letter included) is selected.
fn drag_margin(rect: Rect) -> i32 {
    (rect.height / 2).clamp(3, 14)
}

/// Where a drag selection should *begin* for a phrase: just left of the first
/// glyph, vertically centred (see [`drag_margin`]).
fn drag_start(rect: Rect) -> Point {
    Point::new(rect.x - drag_margin(rect), rect.y + rect.height / 2)
}

/// Where a drag selection should *end* for a phrase: just past the last glyph,
/// vertically centred, so the release sits after the final character and the
/// whole phrase is selected.
fn drag_end(rect: Rect) -> Point {
    Point::new(rect.right() + drag_margin(rect), rect.y + rect.height / 2)
}

/// Estimate the pixel size of a label box for a monospace-ish font.
fn box_size(label: &str, font_size: f64) -> (i32, i32) {
    let pad_x = 4;
    let pad_y = 2;
    // ~0.62 em advance is a good average for monospace digits/letters.
    let char_w = (font_size * 0.62).ceil() as i32;
    let w = label.chars().count() as i32 * char_w + 2 * pad_x;
    let h = font_size.ceil() as i32 + 2 * pad_y;
    (w.max(8), h.max(8))
}

/// Candidate offsets (in box-height units) tried, in order, when the preferred
/// spot is taken. Down first (lists grow downward), then a small grid around.
const OFFSETS: &[(i32, i32)] = &[
    (0, 0),
    (0, 1),
    (1, 0),
    (1, 1),
    (0, -1),
    (-1, 0),
    (0, 2),
    (2, 0),
    (1, -1),
    (-1, 1),
];

/// A request to place one label: where its box prefers to sit (`anchor`, its
/// top-left) and where the pointer should land when it's chosen (`target`).
struct Anchor {
    anchor: Point,
    target: Point,
}

/// Place a hint box for every (element, label) pair, avoiding overlaps where it
/// can. `min_gap` is the minimum spacing enforced between boxes.
pub fn place_hints(
    elements: &[Element],
    labels: &[String],
    font_size: f64,
    min_gap: i32,
    screen: Rect,
) -> Vec<HintBox> {
    place_hints_avoiding(elements, labels, font_size, min_gap, screen, &[])
}

/// Like [`place_hints`], but lay the new boxes out *around* `existing` ones
/// without moving them, used when late-arriving (background-OCR'd) hints are
/// added to an overlay that's already up. Returns only the newly placed boxes;
/// their `index` continues after `existing`.
pub fn place_hints_avoiding(
    elements: &[Element],
    labels: &[String],
    font_size: f64,
    min_gap: i32,
    screen: Rect,
    existing: &[HintBox],
) -> Vec<HintBox> {
    assert_eq!(elements.len(), labels.len(), "one label per element");
    let anchors: Vec<Anchor> = elements
        .iter()
        .map(|el| Anchor {
            anchor: Point::new(el.rect.x, el.rect.y),
            target: text_start(el.rect),
        })
        .collect();
    place_anchors(&anchors, labels, font_size, min_gap, screen, existing)
}

/// Place drag-selection hints: two per phrase, one on its first glyph and one
/// just past its last. Picking the start of one phrase and the end of another
/// then drag-selects the whole span between them. `labels` must hold one label
/// per hint, i.e. `2 * elements.len()`, in the order start₀, end₀, start₁, …
pub fn place_drag_hints(
    elements: &[Element],
    labels: &[String],
    font_size: f64,
    min_gap: i32,
    screen: Rect,
) -> Vec<HintBox> {
    assert_eq!(
        labels.len(),
        elements.len() * 2,
        "two labels per element (start + end)"
    );
    let anchors: Vec<Anchor> = elements
        .iter()
        .flat_map(|el| {
            [
                // Start hint hugs the left edge; end hint sits at the right edge.
                Anchor {
                    anchor: Point::new(el.rect.x, el.rect.y),
                    target: drag_start(el.rect),
                },
                Anchor {
                    anchor: Point::new(el.rect.right(), el.rect.y),
                    target: drag_end(el.rect),
                },
            ]
        })
        .collect();
    place_anchors(&anchors, labels, font_size, min_gap, screen, &[])
}

/// Lay out one label box per [`Anchor`], nudging around `existing` boxes and the
/// ones placed so far to avoid overlaps. Returns only the newly placed boxes;
/// their `index` continues after `existing`.
fn place_anchors(
    anchors: &[Anchor],
    labels: &[String],
    font_size: f64,
    min_gap: i32,
    screen: Rect,
    existing: &[HintBox],
) -> Vec<HintBox> {
    assert_eq!(anchors.len(), labels.len(), "one label per anchor");

    let base_index = existing.len();
    let mut placed: Vec<HintBox> = existing.to_vec();

    for (i, (a, label)) in anchors.iter().zip(labels).enumerate() {
        let (w, h) = box_size(label, font_size);
        let base = a.anchor;

        let mut chosen: Option<Rect> = None;
        for &(dx, dy) in OFFSETS {
            let cand = Rect::new(
                base.x + dx * (w + min_gap),
                base.y + dy * (h + min_gap),
                w,
                h,
            );
            let Some(cand) = cand
                .clamp_to(screen)
                .filter(|c| c.width == w && c.height == h)
            else {
                continue; // partly off-screen at this offset
            };
            let collides = placed
                .iter()
                .any(|p| p.rect.intersects_with_gap(&cand, min_gap));
            if !collides {
                chosen = Some(cand);
                break;
            }
        }

        // If everything collided, fall back to the on-screen clamped base.
        let rect = chosen.unwrap_or_else(|| {
            Rect::new(base.x, base.y, w, h)
                .clamp_to(screen)
                .unwrap_or_else(|| Rect::new(screen.x, screen.y, w, h))
        });

        placed.push(HintBox {
            index: base_index + i,
            label: label.clone(),
            rect,
            target: a.target,
        });
    }

    // Return only the boxes we just added, leaving `existing` untouched.
    placed.split_off(base_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elem(x: i32, y: i32, w: i32, h: i32) -> Element {
        Element::new(Rect::new(x, y, w, h), "t")
    }

    fn no_overlaps(boxes: &[HintBox], gap: i32) -> bool {
        for (i, a) in boxes.iter().enumerate() {
            for (j, b) in boxes.iter().enumerate() {
                if i != j && a.rect.intersects_with_gap(&b.rect, gap) {
                    return false;
                }
            }
        }
        true
    }

    #[test]
    fn target_is_the_start_of_the_text_not_the_centre() {
        let els = [elem(100, 100, 400, 20)];
        let labels = vec!["aa".to_string()];
        let boxes = place_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 1920, 1080));
        // Left edge + half a line height in, vertically centred, near the first
        // glyph, well left of the phrase centre (which would be x=300).
        assert_eq!(boxes[0].target, Point::new(110, 110));
    }

    #[test]
    fn colliding_anchors_are_separated() {
        // Three elements stacked on the exact same spot.
        let els = [
            elem(50, 50, 10, 10),
            elem(50, 50, 10, 10),
            elem(50, 50, 10, 10),
        ];
        let labels = vec!["aa".into(), "ab".into(), "ac".into()];
        let boxes = place_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 1920, 1080));
        assert_eq!(boxes.len(), 3);
        assert!(
            no_overlaps(&boxes, 4),
            "boxes should not overlap after layout"
        );
    }

    #[test]
    fn drag_hints_sit_at_both_ends_of_each_phrase() {
        let els = [elem(100, 100, 400, 20)];
        let labels = vec!["aa".to_string(), "ab".to_string()];
        let boxes = place_drag_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 1920, 1080));
        assert_eq!(boxes.len(), 2, "one start hint and one end hint");
        // Start sits just LEFT of the first glyph (margin = height/2 = 10), so the
        // selection begins before the first character, not inside it.
        assert_eq!(boxes[0].target, Point::new(90, 110));
        // End sits just past the right edge so the release is after the last glyph.
        assert_eq!(boxes[1].target, Point::new(510, 110));
    }

    #[test]
    #[should_panic(expected = "two labels per element")]
    fn drag_hints_require_two_labels_per_phrase() {
        let els = [elem(0, 0, 40, 16)];
        let labels = vec!["aa".to_string()]; // only one, should be two
        place_drag_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 800, 600));
    }

    #[test]
    fn boxes_stay_on_screen() {
        // Element at the far bottom-right corner.
        let els = [elem(1915, 1075, 4, 4)];
        let labels = vec!["aa".to_string()];
        let screen = Rect::new(0, 0, 1920, 1080);
        let boxes = place_hints(&els, &labels, 13.0, 4, screen);
        assert!(boxes[0].rect.clamp_to(screen) == Some(boxes[0].rect));
    }
}
