//! # mole
//!
//! Keyboard-only mouse navigation for Linux/X11. A background daemon waits for a
//! trigger, finds every text/clickable element on screen (via the AT-SPI
//! accessibility tree, falling back to OCR), draws a transparent overlay that
//! labels each one with a couple of letters, and teleports — or clicks, or drags
//! — the pointer to whichever label you type.
//!
//! The crate is split so each concern is testable in isolation:
//!
//! | Module        | Responsibility                                      |
//! |---------------|-----------------------------------------------------|
//! | [`config`]    | TOML config, defaults, hot reload                   |
//! | [`geometry`]  | `Rect`/`Point` primitives                           |
//! | [`capture`]   | Screen capture into a [`capture::Screen`]           |
//! | [`x11`]       | Connection, global hotkey, pointer, overlay window  |
//! | [`detect`]    | AT-SPI and OCR element detection                    |
//! | [`hint`]      | Label generation, matching, anti-overlap layout     |
//! | [`render`]    | Drawing the hint overlay with cairo                 |
//! | [`interaction`] | Clicks, drags, clipboard                          |
//! | [`session`]   | Orchestration of a single hint interaction          |
//! | [`daemon`]    | Background process + Unix-socket IPC                 |

pub mod capture;
pub mod config;
pub mod daemon;
pub mod detect;
pub mod error;
pub mod geometry;
pub mod hint;
pub mod interaction;
pub mod render;
pub mod session;
pub mod x11;

pub use config::Config;
pub use error::{Error, Result};
