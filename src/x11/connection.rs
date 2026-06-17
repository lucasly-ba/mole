//! Thin wrapper around an x11rb connection.
//!
//! Centralises the things every other X11 module needs: the connection itself,
//! the root window and its size, and a keysym→keycode map built once at startup
//! (X grabs and synthetic key events are expressed in keycodes, but humans and
//! config files speak in symbols).

use std::collections::HashMap;

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::error::{Error, Result};
use crate::geometry::Rect;

/// A decoded key event, abstracted away from raw keycodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyInput {
    /// A printable character, plus whether Shift was held. The character is the
    /// key's *unshifted* symbol, so a binding matches the same key with or
    /// without Shift; `shift` is reported separately so it can be used as a
    /// modifier (e.g. large-step movement) on any key, not just letters.
    Char {
        c: char,
        shift: bool,
    },
    Escape,
    Backspace,
    Enter,
    /// Arrow keys, so free-move can be steered with them as well as hjkl.
    Left,
    Right,
    Up,
    Down,
    /// Anything we don't special-case.
    Other,
}

/// Owns the X connection and the lookups derived from it.
pub struct Conn {
    pub conn: RustConnection,
    pub screen_num: usize,
    pub root: u32,
    pub root_width: u16,
    pub root_height: u16,
    keymap: HashMap<u32, u8>,
    // Raw keyboard mapping, kept for decoding incoming key events.
    min_keycode: u8,
    syms_per: usize,
    keysyms: Vec<u32>,
}

impl Conn {
    /// Open the X display named by `$DISPLAY` and prime the keysym maps.
    pub fn open() -> Result<Conn> {
        let (conn, screen_num) = x11rb::connect(None).map_err(Error::x11)?;
        let screen = conn.setup().roots[screen_num].clone();
        let mapping = load_mapping(&conn)?;
        Ok(Conn {
            conn,
            screen_num,
            root: screen.root,
            root_width: screen.width_in_pixels,
            root_height: screen.height_in_pixels,
            keymap: mapping.forward,
            min_keycode: mapping.min_keycode,
            syms_per: mapping.syms_per,
            keysyms: mapping.keysyms,
        })
    }

    /// The keysym produced by `keycode` at shift `level` (0 = unshifted).
    pub fn keysym(&self, keycode: u8, level: usize) -> u32 {
        if keycode < self.min_keycode || self.syms_per == 0 {
            return 0;
        }
        let base = (keycode - self.min_keycode) as usize * self.syms_per;
        self.keysyms.get(base + level).copied().unwrap_or(0)
    }

    /// Decode a raw key press into a [`KeyInput`].
    ///
    /// The character is read at the *unshifted* level so a binding matches its
    /// key regardless of Shift; `shifted` is carried through on
    /// [`KeyInput::Char`] for callers that treat Shift as a modifier.
    pub fn decode_key(&self, keycode: u8, shifted: bool) -> KeyInput {
        let sym = self.keysym(keycode, 0);
        match sym {
            0xff1b => KeyInput::Escape,
            0xff08 => KeyInput::Backspace,
            0xff0d | 0xff8d => KeyInput::Enter, // Return, KP_Enter
            0xff51 => KeyInput::Left,           // arrow keys
            0xff52 => KeyInput::Up,
            0xff53 => KeyInput::Right,
            0xff54 => KeyInput::Down,
            // Latin-1 printable range maps straight to a char.
            0x20..=0x7e | 0xa0..=0xff => char::from_u32(sym)
                .map(|c| KeyInput::Char { c, shift: shifted })
                .unwrap_or(KeyInput::Other),
            _ => KeyInput::Other,
        }
    }

    /// The full root-window rectangle.
    pub fn root_bounds(&self) -> Rect {
        Rect::new(0, 0, self.root_width as i32, self.root_height as i32)
    }

    /// The rectangles of the physically connected monitors, in root coordinates.
    ///
    /// On a multi-head setup the root window is the *bounding box* of all
    /// monitors, which can include regions no monitor actually covers, e.g. the
    /// area below a shorter monitor sitting beside a taller one. The server
    /// returns undefined pixels there, and OCR must not be fed them (a band that
    /// spans such a region recognises nothing). This reports the real per-monitor
    /// rectangles so OCR can stay inside them.
    ///
    /// Falls back to the whole root if RandR is unavailable or reports nothing, so
    /// a single-head (or RandR-less) display behaves exactly as before.
    pub fn monitors(&self) -> Vec<Rect> {
        let rects = self.query_monitors().unwrap_or_default();
        if rects.is_empty() {
            vec![self.root_bounds()]
        } else {
            rects
        }
    }

    /// Query RandR for the active monitor rectangles, clamped to the root. Returns
    /// an error (swallowed by [`Conn::monitors`]) if the extension is missing.
    fn query_monitors(&self) -> Result<Vec<Rect>> {
        use x11rb::protocol::randr::ConnectionExt as _;
        let reply = self
            .conn
            .randr_get_monitors(self.root, true)
            .map_err(Error::x11)?
            .reply()
            .map_err(Error::x11)?;
        let root = self.root_bounds();
        Ok(reply
            .monitors
            .iter()
            .filter_map(|m| {
                Rect::new(m.x as i32, m.y as i32, m.width as i32, m.height as i32).clamp_to(root)
            })
            .collect())
    }

    /// Look up the keycode that currently produces `keysym`, if any.
    pub fn keycode(&self, keysym: u32) -> Option<u8> {
        self.keymap.get(&keysym).copied()
    }

    /// Convenience: keycode for a printable ASCII character. For Latin-1
    /// characters the keysym equals the codepoint.
    pub fn keycode_for_char(&self, c: char) -> Option<u8> {
        self.keycode(keysym_for_char(c))
    }

    pub fn flush(&self) -> Result<()> {
        self.conn.flush().map_err(Error::x11)?;
        Ok(())
    }
}

/// Map a printable character to its X keysym. Latin-1 codepoints map directly.
pub fn keysym_for_char(c: char) -> u32 {
    c as u32
}

/// The keyboard mapping in the two forms the rest of the code needs.
struct Mapping {
    forward: HashMap<u32, u8>,
    min_keycode: u8,
    syms_per: usize,
    keysyms: Vec<u32>,
}

/// Read the server's current keyboard mapping. Builds a keysym→keycode table
/// (for grabs and synthetic input) and keeps the raw table (for decoding events).
/// The first non-zero keysym of each keycode wins the forward lookup, which is
/// what an unmodified key press produces.
fn load_mapping(conn: &RustConnection) -> Result<Mapping> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = max - min + 1;

    let mapping = conn
        .get_keyboard_mapping(min, count)
        .map_err(Error::x11)?
        .reply()
        .map_err(Error::x11)?;

    let per = mapping.keysyms_per_keycode as usize;
    let mut forward = HashMap::new();
    for (i, chunk) in mapping.keysyms.chunks(per.max(1)).enumerate() {
        let keycode = min + i as u8;
        if let Some(&sym) = chunk.iter().find(|&&s| s != 0) {
            forward.entry(sym).or_insert(keycode);
        }
    }
    Ok(Mapping {
        forward,
        min_keycode: min,
        syms_per: per,
        keysyms: mapping.keysyms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_keysym_is_codepoint() {
        assert_eq!(keysym_for_char('a'), 0x61);
        assert_eq!(keysym_for_char('h'), 0x68);
    }
}
