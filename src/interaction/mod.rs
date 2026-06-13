//! High-level pointer interactions and the clipboard (Plan §4).
//!
//! Clicking and dragging themselves live in [`crate::x11::pointer`]; this module
//! adds the clipboard glue for the drag-to-select flow (Plan §4.2: "automatic
//! copy to the clipboard via arboard").
//!
//! On X11 a click-drag puts the selected text into the PRIMARY selection.
//! [`copy_primary_to_clipboard`] mirrors that into the regular CLIPBOARD so a
//! normal paste works afterwards.

use arboard::{Clipboard, GetExtLinux, LinuxClipboardKind, SetExtLinux};

use crate::error::{Error, Result};

/// Copy the current PRIMARY selection into the CLIPBOARD selection.
///
/// Returns the copied text on success. Done as a single helper because both the
/// read and the write can fail independently and we want one tidy error.
pub fn copy_primary_to_clipboard() -> Result<String> {
    let mut clipboard = Clipboard::new().map_err(|e| Error::X11(format!("clipboard: {e}")))?;

    let text = clipboard
        .get()
        .clipboard(LinuxClipboardKind::Primary)
        .text()
        .map_err(|e| Error::X11(format!("reading PRIMARY selection: {e}")))?;

    clipboard
        .set()
        .clipboard(LinuxClipboardKind::Clipboard)
        .text(text.clone())
        .map_err(|e| Error::X11(format!("writing CLIPBOARD: {e}")))?;

    Ok(text)
}

/// Set the CLIPBOARD selection to `text`.
pub fn set_clipboard(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().map_err(|e| Error::X11(format!("clipboard: {e}")))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|e| Error::X11(format!("writing CLIPBOARD: {e}")))?;
    Ok(())
}
