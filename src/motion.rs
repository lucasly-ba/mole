//! Smooth pointer gliding for free (hjkl) mode.
//!
//! Free-move is meant to feel like a real mouse, not a sequence of jumps. So
//! instead of warping a fixed step per key press, [`Glide`] models a *velocity*:
//! while a direction is held the pointer keeps moving, ramping from a base speed
//! up to a cap the longer you hold, and a boost key multiplies the speed for
//! crossing the screen quickly. The session loop ticks this on a fixed timer and
//! warps the pointer by the resulting delta each frame.
//!
//! Speeds are in pixels per second and a tick is a fraction of a second, so a
//! frame's motion is `speed * dt`; sub-pixel remainders are carried between
//! frames so slow speeds still move smoothly rather than stalling. This is pure
//! arithmetic with no X11 in sight, so it is unit-tested directly;
//! `session::run_free_move` owns the keyboard grab and the actual pointer warp.

/// Turns a held direction (and how long it's been held) into smooth per-frame
/// pixel deltas, accelerating while a direction is sustained and stopping the
/// moment nothing is held.
pub struct Glide {
    /// Speed (px/s) the first frame a direction is held.
    base: f64,
    /// Hard ceiling (px/s) however long a direction is held.
    max: f64,
    /// Multiplier applied to the speed for each second a direction stays held
    /// (`1.0` disables acceleration).
    accel: f64,
    /// Current speed (px/s); `0.0` when nothing is held.
    speed: f64,
    /// Sub-pixel remainders carried to the next frame so slow glides still move.
    rem_x: f64,
    rem_y: f64,
}

impl Glide {
    pub fn new(base: f64, max: f64, accel: f64) -> Self {
        Glide {
            base: base.max(0.0),
            max: max.max(base),
            accel: accel.max(1.0),
            speed: 0.0,
            rem_x: 0.0,
            rem_y: 0.0,
        }
    }

    /// Advance one frame. `dx`/`dy` are the held direction on each axis, each in
    /// `{-1, 0, 1}` (so a diagonal is `(±1, ±1)`); `dt` is the frame time in
    /// seconds and `boost` a speed multiplier (`1.0` for none). Returns the whole
    /// pixels to warp the pointer by this frame.
    pub fn tick(&mut self, dx: i32, dy: i32, dt: f64, boost: f64) -> (i32, i32) {
        if dx == 0 && dy == 0 {
            // Released: stop dead and forget any momentum or sub-pixel debt.
            self.speed = 0.0;
            self.rem_x = 0.0;
            self.rem_y = 0.0;
            return (0, 0);
        }

        // First frame of a press starts at the base speed; subsequent frames ramp
        // it up toward the cap by the time held.
        self.speed = if self.speed < self.base {
            self.base
        } else {
            (self.speed * self.accel.powf(dt)).min(self.max)
        };

        // Normalise so a diagonal isn't √2 faster than a straight line.
        let len = ((dx * dx + dy * dy) as f64).sqrt();
        let travel = self.speed * boost.max(1.0) * dt;
        self.rem_x += dx as f64 / len * travel;
        self.rem_y += dy as f64 / len * travel;

        let mx = self.rem_x.trunc();
        let my = self.rem_y.trunc();
        self.rem_x -= mx;
        self.rem_y -= my;
        (mx as i32, my as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_frame_moves_at_the_base_speed() {
        let mut g = Glide::new(1000.0, 4000.0, 2.0);
        assert_eq!(g.tick(1, 0, 0.1, 1.0), (100, 0)); // 1000 px/s * 0.1 s
    }

    #[test]
    fn releasing_stops_and_clears_momentum() {
        let mut g = Glide::new(1000.0, 4000.0, 2.0);
        g.tick(0, 1, 0.5, 1.0); // moving
        assert_eq!(g.tick(0, 0, 0.1, 1.0), (0, 0), "no direction = no motion");
    }

    #[test]
    fn holding_accelerates_up_to_the_cap() {
        let mut g = Glide::new(1000.0, 4000.0, 2.0);
        assert_eq!(g.tick(0, 1, 1.0, 1.0), (0, 1000)); // base
        assert_eq!(g.tick(0, 1, 1.0, 1.0), (0, 2000)); // 1000 * 2^1
        assert_eq!(g.tick(0, 1, 1.0, 1.0), (0, 4000)); // 4000 capped
        assert_eq!(g.tick(0, 1, 1.0, 1.0), (0, 4000)); // stays capped
    }

    #[test]
    fn boost_multiplies_the_speed() {
        let mut g = Glide::new(1000.0, 4000.0, 1.0);
        assert_eq!(g.tick(1, 0, 0.1, 2.0), (200, 0)); // 1000 * 0.1 * 2
    }

    #[test]
    fn a_diagonal_is_normalised() {
        let mut g = Glide::new(1000.0, 4000.0, 1.0);
        // Each axis gets 1000/√2 ≈ 707, not the full 1000.
        assert_eq!(g.tick(1, 1, 1.0, 1.0), (707, 707));
    }

    #[test]
    fn acceleration_of_one_never_speeds_up() {
        let mut g = Glide::new(500.0, 4000.0, 1.0);
        assert_eq!(g.tick(1, 0, 1.0, 1.0), (500, 0));
        assert_eq!(g.tick(1, 0, 1.0, 1.0), (500, 0));
        assert_eq!(g.tick(1, 0, 1.0, 1.0), (500, 0));
    }

    #[test]
    fn sub_pixel_motion_accumulates_then_steps() {
        // 100 px/s for 5 ms = 0.5 px a frame: no move, then a 1 px move.
        let mut g = Glide::new(100.0, 100.0, 1.0);
        assert_eq!(g.tick(1, 0, 0.005, 1.0), (0, 0));
        assert_eq!(g.tick(1, 0, 0.005, 1.0), (1, 0));
    }
}
