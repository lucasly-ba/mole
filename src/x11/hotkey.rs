//! Global hotkeys (Plan §1.2).
//!
//! Uses `XGrabKey` so a key combination fires regardless of which window has
//! focus. Each grab is registered four times to stay robust against the
//! Caps-Lock and Num-Lock modifier bits, which X ORs into the event state.
//!
//! [`HotkeyManager`] is the standalone trigger path: register a combination and
//! block on [`HotkeyManager::wait`] for its action name. It owns its grabs on a
//! connection so they survive independently of the overlay. In practice most
//! users trigger mole via the daemon's Unix socket from their window manager
//! (`exec mole click`), so this path is here for setups that would rather
//! grab the key directly than route through a WM binding.

use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
use x11rb::protocol::Event;

use crate::error::{Error, Result};
use crate::x11::connection::{Conn, Modifiers};

/// A key + modifier combination bound to a named action.
#[derive(Debug, Clone)]
pub struct Hotkey {
    pub keycode: u8,
    pub modifiers: Modifiers,
    pub action: String,
}

/// The lock-modifier bits we must ignore: Caps Lock and Num Lock (Mod2).
const IGNORED_LOCKS: &[u16] = &[
    0,
    0x02,       // Lock (Caps)
    0x10,       // Mod2 (Num)
    0x02 | 0x10, // both
];

/// The set of modifier bits we actually compare against (everything else, like
/// the lock bits, is masked out of incoming events).
fn significant_mask() -> u16 {
    u16::from(ModMask::SHIFT)
        | u16::from(ModMask::CONTROL)
        | u16::from(ModMask::M1)
        | u16::from(ModMask::M4)
}

/// Owns a set of grabs on a connection and dispatches incoming key presses.
pub struct HotkeyManager<'a> {
    conn: &'a Conn,
    bindings: Vec<Hotkey>,
}

impl<'a> HotkeyManager<'a> {
    pub fn new(conn: &'a Conn) -> Self {
        HotkeyManager {
            conn,
            bindings: Vec::new(),
        }
    }

    /// Grab `hotkey` on the root window. Returns an error if the combination is
    /// already grabbed by someone else (X reports this asynchronously, so we
    /// flush and surface any access error).
    pub fn register(&mut self, hotkey: Hotkey) -> Result<()> {
        for &lock in IGNORED_LOCKS {
            self.conn
                .conn
                .grab_key(
                    true,
                    self.conn.root,
                    ModMask::from(hotkey.modifiers.bits() | lock),
                    hotkey.keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(Error::x11)?;
        }
        self.conn.flush()?;
        log::debug!(
            "grabbed keycode {} mods {:#x} -> {}",
            hotkey.keycode,
            hotkey.modifiers.bits(),
            hotkey.action
        );
        self.bindings.push(hotkey);
        Ok(())
    }

    /// Block until a registered hotkey fires, returning its action name.
    /// Unrelated events are ignored.
    pub fn wait(&self) -> Result<String> {
        loop {
            let event = self.conn.conn.wait_for_event().map_err(Error::x11)?;
            if let Event::KeyPress(ev) = event {
                let state = u16::from(ev.state) & significant_mask();
                if let Some(hk) = self
                    .bindings
                    .iter()
                    .find(|hk| hk.keycode == ev.detail && hk.modifiers.bits() == state)
                {
                    return Ok(hk.action.clone());
                }
            }
        }
    }
}

impl Drop for HotkeyManager<'_> {
    fn drop(&mut self) {
        // Release every grab so the keys work normally again after shutdown.
        for hk in &self.bindings {
            for &lock in IGNORED_LOCKS {
                let _ = self.conn.conn.ungrab_key(
                    hk.keycode,
                    self.conn.root,
                    ModMask::from(hk.modifiers.bits() | lock),
                );
            }
        }
        let _ = self.conn.flush();
    }
}
