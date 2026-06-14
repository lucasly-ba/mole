//! # mole
//!
//! Keyboard-only mouse navigation for Linux/X11. A background daemon waits for a
//! trigger, reads every line of text on screen with OCR, draws a transparent
//! overlay that labels each phrase with a couple of letters, and teleports — or
//! clicks, or drags — the pointer to whichever label you type. Between triggers
//! the same daemon also does free `hjkl` pointer movement, no scanning involved.
//!
//! The crate is split so each concern is testable in isolation:
//!
//! | Module        | Responsibility                                      |
//! |---------------|-----------------------------------------------------|
//! | [`config`]    | TOML config, defaults, hot reload                   |
//! | [`geometry`]  | `Rect`/`Point` primitives                           |
//! | [`capture`]   | Screen capture into a [`capture::Screen`]           |
//! | [`x11`]       | Connection, pointer, transparent overlay window     |
//! | [`detect`]    | OCR text detection, grouped into phrase targets     |
//! | [`hint`]      | Label generation, matching, anti-overlap layout     |
//! | [`motion`]    | Accelerating step sizing for free hjkl movement     |
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
pub mod motion;
pub mod render;
pub mod session;
pub mod x11;

pub use config::Config;
pub use error::{Error, Result};
