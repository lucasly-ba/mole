//! A bare keyboard grab for free-move mode (Plan §1.3).
//!
//! Free-move shows nothing of its own: the pointer glides over the *live*
//! desktop and any clicks fall through to the apps beneath. So, unlike the hint
//! [`Overlay`](crate::x11::overlay::Overlay), there is no window and no rendering
//! at all, and therefore none of the "black screen without a compositor"
//! trouble that forced the overlay to paint a frozen screenshot. We simply grab
//! the keyboard on the root window (`owner_events = false`, so every key reaches
//! us regardless of which window has focus), read the key transitions, and warp
//! the real pointer. Dropping the grab hands the keyboard back to the window
//! manager.

use std::thread::sleep;
use std::time::Duration;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, GrabStatus, ModMask};
use x11rb::protocol::Event;
use x11rb::CURRENT_TIME;

use crate::error::{Error, Result};
use crate::x11::connection::{Conn, KeyInput};

/// A key going down (`pressed = true`) or coming back up, with X auto-repeat
/// filtered out so a held key reads as one press until it is really released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyTransition {
    pub input: KeyInput,
    pub pressed: bool,
}

/// An active keyboard grab on the root window. Dropping it releases the grab.
pub struct KeyboardGrab<'a> {
    conn: &'a Conn,
    active: bool,
}

impl<'a> KeyboardGrab<'a> {
    /// Grab the keyboard, retrying briefly in case another grab is momentarily
    /// in the way.
    pub fn new(conn: &'a Conn) -> Result<Self> {
        let mut grab = KeyboardGrab {
            conn,
            active: false,
        };
        for _ in 0..20 {
            let reply = conn
                .conn
                .grab_keyboard(
                    false, // owner_events: deliver every key to us
                    conn.root,
                    CURRENT_TIME,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(Error::x11)?
                .reply()
                .map_err(Error::x11)?;
            if reply.status == GrabStatus::SUCCESS {
                grab.active = true;
                return Ok(grab);
            }
            sleep(Duration::from_millis(10));
        }
        Err(Error::X11("could not grab keyboard for move mode".into()))
    }

    /// Take every key transition currently queued, with X auto-repeat collapsed.
    ///
    /// Without detectable auto-repeat the server fakes a held key as a stream of
    /// `Release`+`Press` pairs sharing a timestamp. Those are not real up/down
    /// events, so a `Release` immediately followed by a same-key `Press` at the
    /// same time is dropped entirely. A held key then yields just its opening
    /// press and its closing release, which is exactly the held-state the move
    /// loop tracks.
    pub fn drain(&self) -> Result<Vec<KeyTransition>> {
        // (pressed, keycode, modifier state, time)
        let mut raw: Vec<RawKey> = Vec::new();
        while let Some(event) = self.conn.conn.poll_for_event().map_err(Error::x11)? {
            match event {
                Event::KeyPress(e) => raw.push((true, e.detail, e.state.into(), e.time)),
                Event::KeyRelease(e) => raw.push((false, e.detail, e.state.into(), e.time)),
                _ => {}
            }
        }
        Ok(collapse_autorepeat(&raw)
            .into_iter()
            .map(|(pressed, keycode, state)| {
                let shifted = state & u16::from(ModMask::SHIFT) != 0;
                KeyTransition {
                    input: self.conn.decode_key(keycode, shifted),
                    pressed,
                }
            })
            .collect())
    }
}

/// One raw key event: `(pressed, keycode, modifier state, time)`.
type RawKey = (bool, u8, u16, u32);

/// Collapse X auto-repeat in a queued run of key events: a release immediately
/// followed by a press of the same keycode at the same timestamp is the server
/// repeating a held key, so both are dropped. Returns the surviving transitions
/// as `(pressed, keycode, state)` (time no longer needed). Pure, so it's tested
/// directly without an X server.
fn collapse_autorepeat(raw: &[RawKey]) -> Vec<(bool, u8, u16)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let (pressed, keycode, state, time) = raw[i];
        if !pressed {
            if let Some(&(next_pressed, next_code, _, next_time)) = raw.get(i + 1) {
                if next_pressed && next_code == keycode && next_time == time {
                    i += 2; // auto-repeat: drop the release and its paired press
                    continue;
                }
            }
        }
        out.push((pressed, keycode, state));
        i += 1;
    }
    out
}

impl Drop for KeyboardGrab<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.conn.conn.ungrab_keyboard(CURRENT_TIME);
            let _ = self.conn.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_press_and_release_survive() {
        let raw = [(true, 40, 0, 100), (false, 40, 0, 250)];
        assert_eq!(
            collapse_autorepeat(&raw),
            vec![(true, 40, 0), (false, 40, 0)]
        );
    }

    #[test]
    fn a_repeat_pair_is_dropped() {
        // Held key: initial press, then release+press pairs sharing a timestamp,
        // then the real release. Only the opening press and final release remain.
        let raw = [
            (true, 40, 0, 100),  // initial press
            (false, 40, 0, 150), // \ auto-repeat (same time)
            (true, 40, 0, 150),  // /
            (false, 40, 0, 150), // \ auto-repeat
            (true, 40, 0, 150),  // /
            (false, 40, 0, 300), // real release (no following press)
        ];
        assert_eq!(
            collapse_autorepeat(&raw),
            vec![(true, 40, 0), (false, 40, 0)],
            "held key reads as one press until truly released"
        );
    }

    #[test]
    fn a_release_followed_by_a_different_key_is_kept() {
        // Not auto-repeat: different keycode, so the release is real.
        let raw = [(false, 40, 0, 100), (true, 41, 0, 100)];
        assert_eq!(
            collapse_autorepeat(&raw),
            vec![(false, 40, 0), (true, 41, 0)]
        );
    }
}
