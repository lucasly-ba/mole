//! Colour helpers for guaranteed hint contrast (Plan §2.2).
//!
//! Two decisions are made per hint:
//!
//! * which text colour reads best on the hint box, and
//! * whether the hint box itself needs nudging to stand out from whatever is
//!   *behind* it on screen.

use crate::config::Rgba;

/// Luminance threshold separating "light" from "dark" backgrounds.
const MID: f64 = 0.55;

/// Pick the text colour that contrasts the box fill: dark text on a light box,
/// light text on a dark box.
pub fn contrasting_text(box_bg: Rgba, dark_text: Rgba, light_text: Rgba) -> Rgba {
    if box_bg.luminance() > MID {
        dark_text
    } else {
        light_text
    }
}

/// If the configured box colour is too close in luminance to what's behind it on
/// screen, push it away (darken a light box over light content, lighten a dark
/// box over dark content) so the box edge stays visible. The hue is preserved.
pub fn adaptive_background(configured: Rgba, behind: Rgba) -> Rgba {
    let diff = (configured.luminance() - behind.luminance()).abs();
    if diff >= 0.2 {
        return configured;
    }
    let [r, g, b, a] = configured.0;
    let factor = if behind.luminance() > MID { 0.55 } else { 1.6 };
    let adj = |c: u8| ((c as f64 * factor).clamp(0.0, 255.0)) as u8;
    Rgba([adj(r), adj(g), adj(b), a])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_box_gets_dark_text() {
        let dark = Rgba::new(0, 0, 0, 255);
        let light = Rgba::new(255, 255, 255, 255);
        let chosen = contrasting_text(Rgba::new(250, 250, 200, 255), dark, light);
        assert_eq!(chosen, dark);
    }

    #[test]
    fn dark_box_gets_light_text() {
        let dark = Rgba::new(0, 0, 0, 255);
        let light = Rgba::new(255, 255, 255, 255);
        let chosen = contrasting_text(Rgba::new(20, 20, 30, 255), dark, light);
        assert_eq!(chosen, light);
    }

    #[test]
    fn distinct_background_is_left_alone() {
        let cfg = Rgba::new(255, 220, 90, 255); // bright yellow
        let behind = Rgba::new(10, 10, 10, 255); // dark
        assert_eq!(adaptive_background(cfg, behind), cfg);
    }

    #[test]
    fn similar_background_is_darkened_over_light_content() {
        let cfg = Rgba::new(240, 240, 240, 255); // light box
        let behind = Rgba::new(230, 230, 230, 255); // light content
        let out = adaptive_background(cfg, behind);
        assert!(out.luminance() < cfg.luminance());
    }
}
