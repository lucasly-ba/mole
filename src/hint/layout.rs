//! Hint box placement with anti-overlap (Plan §2.2).
//!
//! Each detected element gets a small label box anchored at its top-left corner.
//! When boxes would collide (dense UIs, lists), [`place_hints`] nudges later
//! boxes to nearby free positions so labels stay readable. The point the cursor
//! ultimately jumps to is the element centre, kept separately from where the box
//! is drawn.

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

/// Place a hint box for every (element, label) pair, avoiding overlaps where it
/// can. `min_gap` is the minimum spacing enforced between boxes.
pub fn place_hints(
    elements: &[Element],
    labels: &[String],
    font_size: f64,
    min_gap: i32,
    screen: Rect,
) -> Vec<HintBox> {
    assert_eq!(elements.len(), labels.len(), "one label per element");

    let mut placed: Vec<HintBox> = Vec::with_capacity(elements.len());

    for (i, (el, label)) in elements.iter().zip(labels).enumerate() {
        let (w, h) = box_size(label, font_size);
        let base = Point::new(el.rect.x, el.rect.y);

        let mut chosen: Option<Rect> = None;
        for &(dx, dy) in OFFSETS {
            let cand = Rect::new(base.x + dx * (w + min_gap), base.y + dy * (h + min_gap), w, h);
            let Some(cand) = cand.clamp_to(screen).filter(|c| c.width == w && c.height == h) else {
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
            index: i,
            label: label.clone(),
            rect,
            target: el.rect.center(),
        });
    }

    placed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::{Role, Source};

    fn elem(x: i32, y: i32, w: i32, h: i32) -> Element {
        Element::new(Rect::new(x, y, w, h), "t", Role::Link, Source::AtSpi)
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
    fn target_is_element_center_not_box() {
        let els = [elem(100, 100, 40, 20)];
        let labels = vec!["aa".to_string()];
        let boxes = place_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 1920, 1080));
        assert_eq!(boxes[0].target, Point::new(120, 110));
    }

    #[test]
    fn colliding_anchors_are_separated() {
        // Three elements stacked on the exact same spot.
        let els = [elem(50, 50, 10, 10), elem(50, 50, 10, 10), elem(50, 50, 10, 10)];
        let labels = vec!["aa".into(), "ab".into(), "ac".into()];
        let boxes = place_hints(&els, &labels, 13.0, 4, Rect::new(0, 0, 1920, 1080));
        assert_eq!(boxes.len(), 3);
        assert!(no_overlaps(&boxes, 4), "boxes should not overlap after layout");
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
