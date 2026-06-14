//! Small, dependency-free geometry primitives shared across modules.
//!
//! Detection, the hint layout pass and the renderer all speak in terms of
//! [`Rect`] and [`Point`], so they live here in one well-tested place.

use serde::{Deserialize, Serialize};

/// An integer point in screen coordinates (origin top-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }
}

/// An axis-aligned rectangle in screen coordinates.
///
/// Width and height are kept non-negative by construction helpers; callers that
/// build a `Rect` directly are trusted to respect that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// The point a click should target: the rectangle's centre.
    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2, self.y + self.height / 2)
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn area(&self) -> i64 {
        self.width.max(0) as i64 * self.height.max(0) as i64
    }

    /// True when the two rectangles share any interior area.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }

    /// Same as [`Rect::intersects`] but treats rectangles within `gap` pixels of
    /// each other as overlapping. Used by the hint anti-overlap pass.
    pub fn intersects_with_gap(&self, other: &Rect, gap: i32) -> bool {
        let inflated = Rect::new(
            self.x - gap,
            self.y - gap,
            self.width + 2 * gap,
            self.height + 2 * gap,
        );
        inflated.intersects(other)
    }

    /// True when `p` lies inside the rectangle (inclusive of the top-left edge,
    /// exclusive of the bottom-right edge).
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x < self.right() && p.y >= self.y && p.y < self.bottom()
    }

    /// The smallest rectangle that contains both `self` and `other`. Used to
    /// merge the word boxes of a phrase into one hint target.
    pub fn union(&self, other: &Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// Clamp this rectangle to the bounds of `screen`, returning `None` if the
    /// result would be empty (fully off-screen).
    pub fn clamp_to(&self, screen: Rect) -> Option<Rect> {
        let x0 = self.x.max(screen.x);
        let y0 = self.y.max(screen.y);
        let x1 = self.right().min(screen.right());
        let y1 = self.bottom().min(screen.bottom());
        if x1 > x0 && y1 > y0 {
            Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn center_is_midpoint() {
        let r = Rect::new(10, 20, 100, 40);
        assert_eq!(r.center(), Point::new(60, 40));
    }

    #[test]
    fn disjoint_rects_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(20, 20, 10, 10);
        assert!(!a.intersects(&b));
        // ...but a generous gap makes them collide.
        assert!(a.intersects_with_gap(&b, 15));
    }

    #[test]
    fn touching_edges_do_not_intersect() {
        let a = Rect::new(0, 0, 10, 10);
        let b = Rect::new(10, 0, 10, 10);
        assert!(!a.intersects(&b));
    }

    #[test]
    fn clamp_trims_to_screen() {
        let screen = Rect::new(0, 0, 100, 100);
        let r = Rect::new(80, 80, 40, 40);
        assert_eq!(r.clamp_to(screen), Some(Rect::new(80, 80, 20, 20)));

        let offscreen = Rect::new(200, 200, 10, 10);
        assert_eq!(offscreen.clamp_to(screen), None);
    }

    #[test]
    fn union_covers_both_rects() {
        let a = Rect::new(10, 10, 20, 10); // x: 10..30, y: 10..20
        let b = Rect::new(40, 5, 10, 30); // x: 40..50, y: 5..35
        let u = a.union(&b);
        assert_eq!(u, Rect::new(10, 5, 40, 30));
        // Union is commutative and idempotent.
        assert_eq!(b.union(&a), u);
        assert_eq!(a.union(&a), a);
    }

    #[test]
    fn contains_respects_half_open_bounds() {
        let r = Rect::new(0, 0, 10, 10);
        assert!(r.contains(Point::new(0, 0)));
        assert!(!r.contains(Point::new(10, 10)));
        assert!(r.contains(Point::new(9, 9)));
    }
}
