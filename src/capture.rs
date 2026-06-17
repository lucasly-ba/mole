//! Screen capture (Plan §1.1).
//!
//! [`Screen`] is a raw RGBA-ish snapshot of (a region of) the root window plus
//! the geometry it covers. The OCR detector and the renderer's adaptive
//! contrast both read from it. The capture itself uses X11 `GetImage`, which
//! returns server-native pixel data; on the little-endian systems mole
//! targets that is BGRX/BGRA, which [`Screen::pixel`] accounts for.

use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};

use crate::config::Rgba;
use crate::error::{Error, Result};
use crate::geometry::{Point, Rect};
use crate::x11::Conn;

/// A captured rectangle of the screen.
pub struct Screen {
    bounds: Rect,
    /// Pixel data, 4 bytes per pixel, row-major, `stride` bytes per row.
    data: Vec<u8>,
    stride: usize,
    /// Bytes per pixel as reported by the server (usually 4).
    bpp: usize,
}

impl Screen {
    /// Capture the entire root window.
    pub fn capture_full(conn: &Conn) -> Result<Screen> {
        Self::capture_region(conn, conn.root_bounds())
    }

    /// Capture only `region` (Plan §1.1: "only capture the visible zones
    /// needed"). The region is clamped to the root window first.
    pub fn capture_region(conn: &Conn, region: Rect) -> Result<Screen> {
        let region = region
            .clamp_to(conn.root_bounds())
            .ok_or_else(|| Error::X11("capture region is off-screen".into()))?;

        let reply = conn
            .conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                conn.root,
                region.x as i16,
                region.y as i16,
                region.width as u16,
                region.height as u16,
                !0, // all planes
            )
            .map_err(Error::x11)?
            .reply()
            .map_err(Error::x11)?;

        // Z_PIXMAP depth 24/32 is 4 bytes per pixel on the servers we target.
        let bpp = 4;
        let stride = region.width as usize * bpp;
        Ok(Screen {
            bounds: region,
            data: reply.data,
            stride,
            bpp,
        })
    }

    /// Construct a screen from raw bytes. Used by tests and by callers that
    /// already hold pixel data.
    pub fn from_raw(bounds: Rect, data: Vec<u8>, stride: usize, bpp: usize) -> Screen {
        Screen {
            bounds,
            data,
            stride,
            bpp,
        }
    }

    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    pub fn width(&self) -> i32 {
        self.bounds.width
    }

    pub fn height(&self) -> i32 {
        self.bounds.height
    }

    /// Raw pixel bytes, for handing to OCR or other consumers.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// The capture as a tight, top-to-bottom, 3-bytes-per-pixel **RGB** buffer,
    /// the form OCR (and PPM) want. Converts from the server-native BGRX layout
    /// in a single pass over the raw bytes, so it avoids the per-pixel,
    /// bounds-checked sampling of [`Screen::pixel`] on the hot path (this runs on
    /// the full screen for every hint).
    pub fn to_rgb(&self) -> Vec<u8> {
        let w = self.bounds.width.max(0) as usize;
        let h = self.bounds.height.max(0) as usize;
        let mut out = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            let row = y * self.stride;
            for x in 0..w {
                let off = row + x * self.bpp;
                if off + 2 < self.data.len() {
                    out.push(self.data[off + 2]); // R
                    out.push(self.data[off + 1]); // G
                    out.push(self.data[off]); // B
                } else {
                    out.extend_from_slice(&[0, 0, 0]);
                }
            }
        }
        out
    }

    /// Colour at absolute screen coordinate `p`, or opaque black if out of range
    /// (callers sampling for contrast don't want to handle `Option`).
    pub fn pixel(&self, p: Point) -> Rgba {
        let lx = p.x - self.bounds.x;
        let ly = p.y - self.bounds.y;
        if lx < 0 || ly < 0 || lx >= self.bounds.width || ly >= self.bounds.height {
            return Rgba::new(0, 0, 0, 255);
        }
        let off = ly as usize * self.stride + lx as usize * self.bpp;
        if off + 3 >= self.data.len() {
            return Rgba::new(0, 0, 0, 255);
        }
        // Server-native little-endian Z_PIXMAP is B, G, R, X.
        let b = self.data[off];
        let g = self.data[off + 1];
        let r = self.data[off + 2];
        Rgba::new(r, g, b, 255)
    }

    /// Average colour over `rect`, sub-sampled for speed. Drives the renderer's
    /// adaptive text-contrast decision (Plan §2.2: "contrast guaranteed").
    pub fn average_color(&self, rect: Rect) -> Rgba {
        let Some(rect) = rect.clamp_to(self.bounds) else {
            return Rgba::new(128, 128, 128, 255);
        };
        // Sample on a coarse grid: at most ~8x8 probes regardless of size.
        let step_x = (rect.width / 8).max(1);
        let step_y = (rect.height / 8).max(1);
        let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
        let mut y = rect.y;
        while y < rect.bottom() {
            let mut x = rect.x;
            while x < rect.right() {
                let c = self.pixel(Point::new(x, y)).0;
                sr += c[0] as u64;
                sg += c[1] as u64;
                sb += c[2] as u64;
                n += 1;
                x += step_x;
            }
            y += step_y;
        }
        if n == 0 {
            return Rgba::new(128, 128, 128, 255);
        }
        Rgba::new((sr / n) as u8, (sg / n) as u8, (sb / n) as u8, 255)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: i32, height: i32, b: u8, g: u8, r: u8) -> Screen {
        let mut data = vec![0u8; (width * height * 4) as usize];
        for px in data.chunks_mut(4) {
            px[0] = b;
            px[1] = g;
            px[2] = r;
            px[3] = 255;
        }
        Screen::from_raw(
            Rect::new(0, 0, width, height),
            data,
            (width * 4) as usize,
            4,
        )
    }

    #[test]
    fn pixel_reads_bgr_as_rgb() {
        let s = solid(4, 4, 10, 20, 30); // B=10 G=20 R=30
        assert_eq!(s.pixel(Point::new(1, 1)), Rgba::new(30, 20, 10, 255));
    }

    #[test]
    fn out_of_bounds_pixel_is_black() {
        let s = solid(4, 4, 10, 20, 30);
        assert_eq!(s.pixel(Point::new(99, 99)), Rgba::new(0, 0, 0, 255));
    }

    #[test]
    fn average_of_solid_is_that_color() {
        let s = solid(16, 16, 0, 128, 255); // B=0 G=128 R=255
        assert_eq!(
            s.average_color(Rect::new(0, 0, 16, 16)),
            Rgba::new(255, 128, 0, 255)
        );
    }

    #[test]
    fn to_rgb_converts_bgrx_to_tight_rgb() {
        let s = solid(2, 2, 10, 20, 30); // B=10 G=20 R=30
        let rgb = s.to_rgb();
        assert_eq!(rgb.len(), 2 * 2 * 3, "tight 3 bytes per pixel");
        // Every pixel is R,G,B in order.
        assert_eq!(&rgb[0..3], &[30, 20, 10]);
        assert!(rgb.chunks(3).all(|px| px == [30, 20, 10]));
    }
}
