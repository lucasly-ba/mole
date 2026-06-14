//! Pointer-movement sizing for free (hjkl) mode.
//!
//! Tapping a direction nudges the pointer by the base step; holding it (the key
//! auto-repeats, arriving as a run of same-direction presses) ramps the step up
//! by a configurable factor on each repeat, capped, so you cross the screen fast
//! but still land precisely. Shift selects a larger base step.
//!
//! This is pure arithmetic with no X11 in sight, so it is unit-tested directly;
//! `session::run_free_move` owns the keyboard loop and the actual pointer warp.

/// A direction of travel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Turns a stream of directional presses into pixel deltas, accelerating while
/// the same direction repeats and resetting the moment it changes.
pub struct Accelerator {
    base: i32,
    large: i32,
    factor: f64,
    max: i32,
    last: Option<Dir>,
    repeats: u32,
}

impl Accelerator {
    /// * `base` / `large` — step for a tap, normally and with Shift.
    /// * `factor` — multiplier applied per consecutive same-direction repeat
    ///   (`1.0` disables acceleration).
    /// * `max` — hard ceiling on a single step.
    pub fn new(base: i32, large: i32, factor: f64, max: i32) -> Self {
        Accelerator {
            base,
            large,
            factor,
            max,
            last: None,
            repeats: 0,
        }
    }

    /// The `(dx, dy)` for one press in `dir`. `large` picks the Shift step.
    pub fn next(&mut self, dir: Dir, large: bool) -> (i32, i32) {
        if self.last == Some(dir) {
            self.repeats = self.repeats.saturating_add(1);
        } else {
            self.repeats = 0;
            self.last = Some(dir);
        }

        let base = if large { self.large } else { self.base };
        let scaled = (base as f64 * self.factor.powi(self.repeats as i32)).round() as i32;
        let step = scaled.clamp(base, self.max.max(base));

        match dir {
            Dir::Left => (-step, 0),
            Dir::Right => (step, 0),
            Dir::Up => (0, -step),
            Dir::Down => (0, step),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_press_is_the_base_step() {
        let mut a = Accelerator::new(24, 160, 1.5, 600);
        assert_eq!(a.next(Dir::Right, false), (24, 0));
        assert_eq!(a.next(Dir::Up, true), (0, -160), "Shift uses the large step");
    }

    #[test]
    fn repeating_a_direction_accelerates() {
        let mut a = Accelerator::new(10, 100, 2.0, 10_000);
        assert_eq!(a.next(Dir::Right, false), (10, 0)); // repeats 0: 10 * 2^0
        assert_eq!(a.next(Dir::Right, false), (20, 0)); // repeats 1: 10 * 2^1
        assert_eq!(a.next(Dir::Right, false), (40, 0)); // repeats 2: 10 * 2^2
    }

    #[test]
    fn changing_direction_resets_acceleration() {
        let mut a = Accelerator::new(10, 100, 2.0, 10_000);
        a.next(Dir::Right, false);
        a.next(Dir::Right, false); // now accelerated
        assert_eq!(a.next(Dir::Left, false), (-10, 0), "new direction starts at base");
    }

    #[test]
    fn step_is_capped_at_max() {
        let mut a = Accelerator::new(10, 100, 10.0, 50);
        assert_eq!(a.next(Dir::Down, false), (0, 10));
        assert_eq!(a.next(Dir::Down, false), (0, 50), "100 clamped to max 50");
        assert_eq!(a.next(Dir::Down, false), (0, 50));
    }

    #[test]
    fn factor_one_never_accelerates() {
        let mut a = Accelerator::new(24, 160, 1.0, 600);
        assert_eq!(a.next(Dir::Right, false), (24, 0));
        assert_eq!(a.next(Dir::Right, false), (24, 0));
        assert_eq!(a.next(Dir::Right, false), (24, 0));
    }
}
