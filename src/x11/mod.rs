//! Everything that talks directly to the X server.
//!
//! * [`connection`]: the shared connection wrapper and key lookups.
//! * [`pointer`]: synthetic motion and clicks via warp/XTest (Plan §1.3, §4.1).
//! * [`overlay`]: the transparent fullscreen overlay window (Plan §2.1).
//! * [`grab`]: a windowless keyboard grab for free-move mode (Plan §1.3).

pub mod connection;
pub mod grab;
pub mod overlay;
pub mod pointer;

pub use connection::Conn;
pub use grab::{KeyTransition, KeyboardGrab};
pub use overlay::{Hud, Overlay};
pub use pointer::{Button, Pointer};
