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

/// The opaque desktop snapshot painted behind the hints, built once per
/// interaction and reused for every repaint. Constructing it is the expensive
/// part of a frame (a per-pixel pass over the whole screen), so caching it keeps
/// each keystroke's redraw to a cheap blit-plus-labels and shrinks the gap
/// between mapping the overlay and the first paint.
pub struct Backdrop {
    surface: ImageSurface,
    width: i32,
    height: i32,
}

/// Renders hint overlays.
pub struct Renderer {
    style: Hints,
}

impl Renderer {
    pub fn new(style: Hints) -> Self {
        Renderer { style }
    }

    /// Build the opaque desktop backdrop from `screen`. Do this once per
    /// interaction; pass the result to [`Renderer::render_onto`] for each frame.
    ///
    /// Painting the captured screen as an opaque backdrop is what lets the
    /// overlay work with no compositor: leaving it transparent only shows the
    /// desktop when a compositor is running to blend the alpha, otherwise the
    /// user gets a black screen with floating labels and no sense of where a jump
    /// lands. Our own snapshot shows them exactly what they're aiming at.
    pub fn backdrop(&self, width: i32, height: i32, screen: &Screen) -> Result<Backdrop> {
        Ok(Backdrop {
            surface: screen_backdrop(width, height, screen, self.style.dim)?,
            width,
            height,
        })
    }

    /// Render the hints that are still consistent with `typed` over a prebuilt
    /// `backdrop`. Boxes whose label does not start with `typed` are skipped,
    /// which is the visual side of progressive matching. `screen` is read for the
    /// adaptive box contrast.
    pub fn render_onto(
        &self,
        backdrop: &Backdrop,
        boxes: &[HintBox],
        typed: &str,
        screen: &Screen,
    ) -> Result<Frame> {
        let width = backdrop.width;
        let height = backdrop.height;
        let mut surface =
            ImageSurface::create(Format::ARgb32, width, height).map_err(Error::render)?;
        {
            let ctx = Context::new(&surface).map_err(Error::render)?;

            // Blit the cached snapshot, then draw the labels on top.
            ctx.set_source_surface(&backdrop.surface, 0.0, 0.0)
                .map_err(Error::render)?;
            ctx.set_operator(Operator::Source);
            ctx.paint().map_err(Error::render)?;
            ctx.set_operator(Operator::Over);

            ctx.select_font_face(&self.style.font_family, FontSlant::Normal, FontWeight::Bold);
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

    /// Convenience: build the backdrop and render `boxes` over it in one call.
    /// Used by tests; the daemon caches the backdrop across repaints instead.
    pub fn render(
        &self,
        width: i32,
        height: i32,
        boxes: &[HintBox],
        typed: &str,
        screen: &Screen,
    ) -> Result<Frame> {
        let backdrop = self.backdrop(width, height, screen)?;
        self.render_onto(&backdrop, boxes, typed, screen)
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
        rounded_rect(
            ctx,
            r.x as f64,
            r.y as f64,
            r.width as f64,
            r.height as f64,
            3.0,
        );
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

/// Build an opaque ARGB32 surface from the captured screen, to paint behind the
/// hints. `dim` (`0.0..=1.0`) darkens it so the labels stand out. Any overlay
/// area not covered by the capture is left opaque black.
fn screen_backdrop(width: i32, height: i32, screen: &Screen, dim: f64) -> Result<ImageSurface> {
    let stride = Format::ARgb32
        .stride_for_width(width.max(0) as u32)
        .map_err(Error::render)?;
    let mut buf = vec![0u8; stride as usize * height.max(0) as usize];

    let keep = 1.0 - dim.clamp(0.0, 1.0);
    let sw = screen.width().max(0);
    let sh = screen.height().max(0);
    // `to_rgb` is a tight, top-to-bottom R,G,B buffer in the screen's own size.
    let rgb = screen.to_rgb();

    for y in 0..height {
        let dst_row = y as usize * stride as usize;
        let in_y = y < sh;
        for x in 0..width {
            let d = dst_row + x as usize * 4;
            if in_y && x < sw {
                let s = (y as usize * sw as usize + x as usize) * 3;
                // cairo ARGB32 is little-endian B,G,R,A; alpha is 255 so the
                // premultiplied and straight forms coincide.
                buf[d] = (rgb[s + 2] as f64 * keep) as u8; // B
                buf[d + 1] = (rgb[s + 1] as f64 * keep) as u8; // G
                buf[d + 2] = (rgb[s] as f64 * keep) as u8; // R
            }
            buf[d + 3] = 255; // opaque
        }
    }

    ImageSurface::create_for_data(buf, Format::ARgb32, width, height, stride).map_err(Error::render)
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
        Screen::from_raw(
            Rect::new(0, 0, 100, 100),
            vec![0u8; 100 * 100 * 4],
            100 * 4,
            4,
        )
    }

    fn solid_screen(b: u8, g: u8, r: u8) -> Screen {
        let mut data = vec![0u8; 100 * 100 * 4];
        for px in data.chunks_mut(4) {
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = 255;
        }
        Screen::from_raw(Rect::new(0, 0, 100, 100), data, 100 * 4, 4)
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
    fn backdrop_is_opaque_and_shows_the_screen() {
        // No boxes: the frame is just the backdrop. Every pixel must be opaque
        // (so it works without a compositor) and reflect the captured screen,
        // darkened by `dim`.
        let r = Renderer::new(Hints::default());
        let frame = r
            .render(100, 100, &[], "", &solid_screen(0, 0, 200))
            .unwrap();
        for px in frame.data.chunks(4) {
            assert_eq!(px[3], 255, "backdrop must be fully opaque");
            // Little-endian ARGB32: red lives in byte 2, dimmed from 200.
            assert!(px[2] > 0 && px[2] < 200, "red should be present but dimmed");
        }
    }

    #[test]
    fn nonmatching_prefix_draws_only_the_backdrop() {
        // Typed "z" matches no label, so the frame equals the bare backdrop.
        let r = Renderer::new(Hints::default());
        let screen = solid_screen(40, 40, 40);
        let with = r.render(100, 100, &[hint("aa")], "z", &screen).unwrap();
        let backdrop_only = r.render(100, 100, &[], "", &screen).unwrap();
        assert_eq!(with.data, backdrop_only.data);
    }

    #[test]
    fn matching_prefix_draws_over_the_backdrop() {
        let r = Renderer::new(Hints::default());
        let screen = solid_screen(40, 40, 40);
        let with = r.render(100, 100, &[hint("aa")], "a", &screen).unwrap();
        let backdrop_only = r.render(100, 100, &[], "", &screen).unwrap();
        assert_ne!(with.data, backdrop_only.data);
    }
}
