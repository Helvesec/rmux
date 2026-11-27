//! Portable Kitty keyboard protocol primitives.
//!
//! This module is deliberately independent of runtime routing, native key
//! acquisition, and RMUX's existing key encoder. It parses already-delimited
//! complete frames, incrementally frames optional acquired key input, and
//! stores only keyboard enhancement state selected by an external screen
//! owner.
//!
//! ```
//! use rmux_core::kitty_keyboard::{encode_event, parse_event_frame, Key};
//!
//! let event = parse_event_frame(b"\x1b[97;2u")?;
//! assert_eq!(event.key, Key::Scalar('a'));
//! assert_eq!(encode_event(&event, 8)?, b"\x1b[97;2u");
//! # Ok::<(), rmux_core::kitty_keyboard::Error>(())
//! ```

mod acquired;
mod decode;
mod encode;
mod event;
mod functions;
mod state;

pub use acquired::{AcquiredDecoder, AcquiredItem, Rejection};
pub use decode::{parse_control_frame, parse_event_frame};
pub use encode::encode_event;
pub use event::{Error, Event, Function, Key, Kind, Text, FRAME_LIMIT, STACK_LIMIT, TEXT_LIMIT};
pub use state::{Apply, Control, Effect, PaneKeyboard, Screen, KNOWN_FLAGS};

#[cfg(test)]
mod tests;
