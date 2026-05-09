//! Bounded event buffers used by live server subscribers.

/// Subscription cursor state and gap accounting.
pub mod cursor;
/// Per-pane output ring and recent live buffer storage.
pub mod ring;

pub use cursor::{OutputCursor, OutputCursorItem, OutputGap};
pub use ring::{
    OutputEvent, OutputRing, RecentOutputSnapshot, DEFAULT_OUTPUT_RING_CAPACITY,
    DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
};
