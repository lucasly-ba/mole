//! Everything that talks directly to the X server.
//!
//! * [`connection`] — the shared connection wrapper and key lookups.
//! * [`pointer`] — synthetic motion and clicks via warp/XTest (Plan §1.3, §4.1).
//! * [`overlay`] — the transparent fullscreen overlay window (Plan §2.1).

pub mod connection;
pub mod overlay;
pub mod pointer;

pub use connection::Conn;
pub use overlay::Overlay;
pub use pointer::{Button, Pointer};
