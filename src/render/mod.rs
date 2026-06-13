//! Overlay rendering with cairo (Plan §2.2).
//!
//! Draws the hint boxes into an in-memory ARGB32 surface and hands the raw bytes
//! back to the caller, who uploads them to the overlay window. Using cairo's
//! image surface (rather than an XCB surface) keeps cairo-xcb out of the build
//! and makes the renderer trivially unit-testable: drawing never needs a display.

pub mod palette;

use cairo::{Context, FontSlant, FontWeight, Format, ImageSurface, Operator};

use crate::capture::Screen;
use crate::config::Hints;
use crate::error::{Error, Result};
use crate::hint::HintBox;

/// A finished frame: premultiplied ARGB32 pixels plus the row stride.
pub struct Frame {
    pub data: Vec<u8>,
    pub stride: usize,
    pub width: i32,
    pub height: i32,
}

/// Renders hint overlays.
pub struct Renderer {
    style: Hints,
}

impl Renderer {
    pub fn new(style: Hints) -> Self {
        Renderer { style }
    }

    /// Render the hints that are still consistent with `typed`.
    ///
    /// `screen` is the captured frame behind the overlay, used to keep boxes
    /// readable over whatever they cover. Boxes whose label does not start with
    /// `typed` are skipped, which is the visual side of progressive matching.
    pub fn render(
        &self,
        width: i32,
        height: i32,
        boxes: &[HintBox],
        typed: &str,
        screen: &Screen,
    ) -> Result<Frame> {
        let mut surface =
            ImageSurface::create(Format::ARgb32, width, height).map_err(Error::render)?;
        {
            let ctx = Context::new(&surface).map_err(Error::render)?;

            // Start fully transparent.
            ctx.set_operator(Operator::Clear);
            ctx.paint().map_err(Error::render)?;
            ctx.set_operator(Operator::Over);

            ctx.select_font_face(
                &self.style.font_family,
                FontSlant::Normal,
                FontWeight::Bold,
            );
            ctx.set_font_size(self.style.font_size);

            for b in boxes {
                if !b.label.starts_with(typed) {
                    continue;
                }
                self.draw_box(&ctx, b, typed, screen)?;
            }
        }

        surface.flush();
        let stride = surface.stride() as usize;
        let data = surface
            .data()
            .map_err(|e| Error::Render(format!("cannot access surface data: {e}")))?
            .to_vec();

        Ok(Frame {
            data,
            stride,
            width,
            height,
        })
    }

    fn draw_box(&self, ctx: &Context, b: &HintBox, typed: &str, screen: &Screen) -> Result<()> {
        let r = b.rect;
        let behind = screen.average_color(r);

        // An in-progress match gets the "matched" colour; otherwise the normal
        // background, nudged for contrast against the content behind it.
        let bg = if !typed.is_empty() {
            self.style.matched
        } else {
            palette::adaptive_background(self.style.background, behind)
        };
        let text_color =
            palette::contrasting_text(bg, self.style.foreground, self.style.foreground_dark);

        // Box fill.
        let (br, bgc, bb, ba) = bg.as_f64();
        ctx.set_source_rgba(br, bgc, bb, ba);
        rounded_rect(ctx, r.x as f64, r.y as f64, r.width as f64, r.height as f64, 3.0);
        ctx.fill().map_err(Error::render)?;

        // Centred label.
        let extents = ctx.text_extents(&b.label).map_err(Error::render)?;
        let tx = r.x as f64 + (r.width as f64 - extents.width()) / 2.0 - extents.x_bearing();
        let ty = r.y as f64 + (r.height as f64 - extents.height()) / 2.0 - extents.y_bearing();
        let (tr, tg, tb, ta) = text_color.as_f64();
        ctx.set_source_rgba(tr, tg, tb, ta);
        ctx.move_to(tx, ty);
        ctx.show_text(&b.label).map_err(Error::render)?;

        Ok(())
    }
}

/// Append a rounded-rectangle path to the current context.
fn rounded_rect(ctx: &Context, x: f64, y: f64, w: f64, h: f64, radius: f64) {
    let r = radius.min(w / 2.0).min(h / 2.0);
    let deg = std::f64::consts::PI / 180.0;
    ctx.new_sub_path();
    ctx.arc(x + w - r, y + r, r, -90.0 * deg, 0.0);
    ctx.arc(x + w - r, y + h - r, r, 0.0, 90.0 * deg);
    ctx.arc(x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
    ctx.arc(x + r, y + r, r, 180.0 * deg, 270.0 * deg);
    ctx.close_path();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Hints;
    use crate::geometry::{Point, Rect};

    fn blank_screen() -> Screen {
        Screen::from_raw(Rect::new(0, 0, 100, 100), vec![0u8; 100 * 100 * 4], 100 * 4, 4)
    }

    fn hint(label: &str) -> HintBox {
        HintBox {
            index: 0,
            label: label.to_string(),
            rect: Rect::new(10, 10, 24, 16),
            target: Point::new(22, 18),
        }
    }

    #[test]
    fn renders_a_frame_of_expected_size() {
        let r = Renderer::new(Hints::default());
        let frame = r
            .render(100, 100, &[hint("aa")], "", &blank_screen())
            .unwrap();
        assert_eq!(frame.width, 100);
        assert_eq!(frame.height, 100);
        assert!(frame.stride >= 100 * 4);
        assert_eq!(frame.data.len(), frame.stride * 100);
    }

    #[test]
    fn nonmatching_prefix_draws_nothing() {
        let r = Renderer::new(Hints::default());
        // Typed "z" matches no label, so the surface stays fully transparent.
        let frame = r
            .render(100, 100, &[hint("aa")], "z", &blank_screen())
            .unwrap();
        assert!(
            frame.data.iter().all(|&byte| byte == 0),
            "no pixels should be drawn for a non-matching prefix"
        );
    }

    #[test]
    fn matching_prefix_draws_something() {
        let r = Renderer::new(Hints::default());
        let frame = r
            .render(100, 100, &[hint("aa")], "a", &blank_screen())
            .unwrap();
        assert!(frame.data.iter().any(|&byte| byte != 0));
    }
}
