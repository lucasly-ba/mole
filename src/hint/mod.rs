//! Hints: turning detected elements into typeable labels and back into a target.
//!
//! * [`label`]: generate prefix-free labels and match keystrokes against them.
//! * [`layout`]: place label boxes on screen without overlapping.

pub mod label;
pub mod layout;

pub use label::{generate_labels, HintMatcher, MatchState};
pub use layout::{place_drag_hints, place_hints, place_hints_avoiding, HintBox};
