//! The transparent fullscreen overlay window (Plan §2.1).
//!
//! Creates a 32-bit ARGB, borderless, override-redirect window the size of the
//! root. Override-redirect keeps the window manager's hands off it (no frame, no
//! tiling), and the alpha channel lets a running compositor show the desktop
//! through the gaps between hint boxes. While shown, the overlay grabs the
//! keyboard so every keystroke goes to hint selection instead of the app below.
//!
//! Rendering is decoupled: callers draw into a buffer (see [`crate::render`]) and
//! hand it to [`Overlay::present`], which uploads it with `PutImage`.

use std::thread::sleep;
use std::time::Duration;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{
    ColormapAlloc, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, GrabMode,
    GrabStatus, ImageFormat, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::CURRENT_TIME;

use crate::error::{Error, Result};
use crate::geometry::Point;
use crate::x11::connection::{Conn, KeyInput};

/// An input event delivered while the overlay is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayInput {
    Key(KeyInput),
    /// A mouse button was pressed somewhere on the overlay.
    Click(Point),
}

/// A live overlay window. Dropping it destroys the window.
pub struct Overlay<'a> {
    conn: &'a Conn,
    window: u32,
    gc: u32,
    width: u16,
    height: u16,
    shown: bool,
}

impl<'a> Overlay<'a> {
    /// Create (but do not yet map) a fullscreen ARGB overlay.
    pub fn new(conn: &'a Conn) -> Result<Self> {
        let screen = &conn.conn.setup().roots[conn.screen_num];
        let (depth, visual) = find_argb_visual(screen)
            .ok_or_else(|| Error::Render("no 32-bit ARGB visual available".into()))?;

        let colormap = conn.conn.generate_id().map_err(Error::x11)?;
        conn.conn
            .create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual)
            .map_err(Error::x11)?;

        let window = conn.conn.generate_id().map_err(Error::x11)?;
        let width = conn.root_width;
        let height = conn.root_height;

        // border_pixel and colormap are mandatory when the window's visual
        // differs from the parent's, else the server returns BadMatch.
        let aux = CreateWindowAux::new()
            .background_pixel(0)
            .border_pixel(0)
            .colormap(colormap)
            .override_redirect(1)
            .event_mask(
                EventMask::EXPOSURE
                    | EventMask::KEY_PRESS
                    | EventMask::BUTTON_PRESS
                    | EventMask::STRUCTURE_NOTIFY,
            );

        conn.conn
            .create_window(
                depth,
                window,
                screen.root,
                0,
                0,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                visual,
                &aux,
            )
            .map_err(Error::x11)?;

        let gc = conn.conn.generate_id().map_err(Error::x11)?;
        conn.conn
            .create_gc(gc, window, &CreateGCAux::new())
            .map_err(Error::x11)?;

        // The colormap is now referenced by the window; we can free our handle.
        conn.conn.free_colormap(colormap).map_err(Error::x11)?;
        conn.flush()?;

        Ok(Overlay {
            conn,
            window,
            gc,
            width,
            height,
            shown: false,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Map the overlay and grab the keyboard so we own all input.
    pub fn show(&mut self) -> Result<()> {
        self.conn.conn.map_window(self.window).map_err(Error::x11)?;
        self.conn.flush()?;
        self.grab_keyboard()?;
        self.shown = true;
        Ok(())
    }

    /// Unmap the overlay and release the keyboard grab.
    pub fn hide(&mut self) -> Result<()> {
        if self.shown {
            let _ = self.conn.conn.ungrab_keyboard(CURRENT_TIME);
            self.conn
                .conn
                .unmap_window(self.window)
                .map_err(Error::x11)?;
            self.conn.flush()?;
            self.shown = false;
        }
        Ok(())
    }

    /// Upload an ARGB32 buffer to the window. `stride` is bytes per row; rows are
    /// repacked tightly if it differs from `width * 4`. The upload is split into
    /// horizontal bands to stay well under the maximum request size.
    pub fn present(&self, data: &[u8], stride: usize) -> Result<()> {
        let w = self.width as usize;
        let h = self.height as usize;
        let tight = w * 4;

        // At most ~256 KiB of pixels per PutImage request.
        let band_rows = (262_144 / tight).max(1);
        let mut row = 0usize;
        let mut scratch = vec![0u8; tight * band_rows.min(h)];

        while row < h {
            let rows = band_rows.min(h - row);
            let needed = tight * rows;
            let band = &mut scratch[..needed];
            for r in 0..rows {
                let src = (row + r) * stride;
                let dst = r * tight;
                band[dst..dst + tight].copy_from_slice(&data[src..src + tight]);
            }
            self.conn
                .conn
                .put_image(
                    ImageFormat::Z_PIXMAP,
                    self.window,
                    self.gc,
                    self.width,
                    rows as u16,
                    0,
                    row as i16,
                    0,
                    32,
                    band,
                )
                .map_err(Error::x11)?;
            row += rows;
        }
        self.conn.flush()?;
        Ok(())
    }

    /// Block for the next overlay input, ignoring uninteresting events.
    pub fn next_input(&self) -> Result<OverlayInput> {
        loop {
            let event = self.conn.conn.wait_for_event().map_err(Error::x11)?;
            match event {
                Event::KeyPress(ev) => {
                    let shifted = u16::from(ev.state)
                        & u16::from(x11rb::protocol::xproto::ModMask::SHIFT)
                        != 0;
                    return Ok(OverlayInput::Key(self.conn.decode_key(ev.detail, shifted)));
                }
                Event::ButtonPress(ev) => {
                    return Ok(OverlayInput::Click(Point::new(
                        ev.root_x as i32,
                        ev.root_y as i32,
                    )));
                }
                _ => continue,
            }
        }
    }

    /// Grab the keyboard, retrying briefly: the window may not be viewable the
    /// instant after `map_window`.
    fn grab_keyboard(&self) -> Result<()> {
        for _ in 0..20 {
            let reply = self
                .conn
                .conn
                .grab_keyboard(
                    false,
                    self.window,
                    CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(Error::x11)?
                .reply()
                .map_err(Error::x11)?;
            if reply.status == GrabStatus::SUCCESS {
                return Ok(());
            }
            sleep(Duration::from_millis(10));
        }
        Err(Error::X11("could not grab keyboard for overlay".into()))
    }
}

impl Drop for Overlay<'_> {
    fn drop(&mut self) {
        let _ = self.hide();
        let _ = self.conn.conn.free_gc(self.gc);
        let _ = self.conn.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

/// Find a 32-bit (ARGB) visual on the screen, returning `(depth, visual_id)`.
fn find_argb_visual(screen: &x11rb::protocol::xproto::Screen) -> Option<(u8, u32)> {
    screen
        .allowed_depths
        .iter()
        .find(|d| d.depth == 32)
        .and_then(|d| d.visuals.first().map(|v| (32u8, v.visual_id)))
}
