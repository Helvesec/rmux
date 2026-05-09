//! Bounded event buffers used by live server subscribers.

/// Subscription cursor state and gap accounting.
pub mod cursor;
/// Live pane-output subscription registry and cap accounting.
pub mod registry;
/// Per-pane output ring and recent live buffer storage.
pub mod ring;

pub use cursor::{OutputCursor, OutputCursorItem, OutputGap};
pub use registry::{
    OutputSubscriptionRecord, PaneOutputSubscriptionKey, SubscriptionLimitError,
    SubscriptionLimits, SubscriptionRegistry, DEFAULT_MAX_SUBSCRIPTIONS_PER_CONNECTION,
    DEFAULT_MAX_SUBSCRIPTIONS_PER_PANE, DEFAULT_SUBSCRIPTION_BATCH_EVENTS,
    DEFAULT_SUBSCRIPTION_STALE_TTL,
};
pub use ring::{
    OutputEvent, OutputRing, RecentOutputSnapshot, DEFAULT_OUTPUT_RING_CAPACITY,
    DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
};
