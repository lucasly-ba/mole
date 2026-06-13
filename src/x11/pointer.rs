//! Synthetic pointer control (Plan §1.3 and §4.1).
//!
//! Absolute teleports use core `WarpPointer` (reliable, no extension needed);
//! button presses/releases and drags use the XTest extension, which injects
//! events the rest of the system treats as real hardware input.

use std::thread::sleep;
use std::time::Duration;

use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::NONE;

use crate::error::{Error, Result};
use crate::geometry::Point;
use crate::x11::connection::Conn;

// XTest event type codes (mirror the core X event numbers).
const BUTTON_PRESS: u8 = 4;
const BUTTON_RELEASE: u8 = 5;

/// A mouse button, numbered as X expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn code(self) -> u8 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        }
    }
}

/// Drives the X pointer.
pub struct Pointer<'a> {
    conn: &'a Conn,
}

impl<'a> Pointer<'a> {
    pub fn new(conn: &'a Conn) -> Self {
        Pointer { conn }
    }

    /// Current pointer position in root coordinates.
    pub fn position(&self) -> Result<Point> {
        let reply = self
            .conn
            .conn
            .query_pointer(self.conn.root)
            .map_err(Error::x11)?
            .reply()
            .map_err(Error::x11)?;
        Ok(Point::new(reply.root_x as i32, reply.root_y as i32))
    }

    /// Teleport the pointer to an absolute screen position.
    pub fn move_to(&self, p: Point) -> Result<()> {
        let p = self.clamp(p);
        self.conn
            .conn
            .warp_pointer(NONE, self.conn.root, 0, 0, 0, 0, p.x as i16, p.y as i16)
            .map_err(Error::x11)?;
        self.conn.flush()?;
        Ok(())
    }

    /// Move the pointer by a relative offset (Plan §1.3: hjkl stepping).
    pub fn move_relative(&self, dx: i32, dy: i32) -> Result<()> {
        let cur = self.position()?;
        self.move_to(Point::new(cur.x + dx, cur.y + dy))
    }

    /// Press and release `button` `count` times in place (Plan §4.1:
    /// `digit + space` repeats the click N times; `count = 2` is a double-click).
    pub fn click(&self, button: Button, count: u32) -> Result<()> {
        for _ in 0..count.max(1) {
            self.press(button)?;
            self.release(button)?;
            // Short gap so consecutive clicks register as distinct events.
            sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    pub fn press(&self, button: Button) -> Result<()> {
        self.fake_button(BUTTON_PRESS, button)
    }

    pub fn release(&self, button: Button) -> Result<()> {
        self.fake_button(BUTTON_RELEASE, button)
    }

    /// Press at `from`, move to `to`, release: a click-drag selection
    /// (Plan §4.2).
    pub fn drag(&self, from: Point, to: Point, button: Button) -> Result<()> {
        self.move_to(from)?;
        sleep(Duration::from_millis(30));
        self.press(button)?;
        sleep(Duration::from_millis(30));
        // Move in a couple of steps; some apps only start a selection once they
        // see motion after the press.
        let mid = Point::new((from.x + to.x) / 2, (from.y + to.y) / 2);
        self.move_to(mid)?;
        sleep(Duration::from_millis(20));
        self.move_to(to)?;
        sleep(Duration::from_millis(30));
        self.release(button)?;
        Ok(())
    }

    fn fake_button(&self, event_type: u8, button: Button) -> Result<()> {
        self.conn
            .conn
            .xtest_fake_input(event_type, button.code(), 0, NONE, 0, 0, 0)
            .map_err(Error::x11)?;
        self.conn.flush()?;
        Ok(())
    }

    fn clamp(&self, p: Point) -> Point {
        let b = self.conn.root_bounds();
        Point::new(
            p.x.clamp(b.x, b.right() - 1),
            p.y.clamp(b.y, b.bottom() - 1),
        )
    }
}
