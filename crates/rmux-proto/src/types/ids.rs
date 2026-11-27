//! Connection-scoped subscription and SDK wait identifiers.

use serde::{Deserialize, Serialize};

/// Stable identifier for one pane-output subscription on a live server
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneOutputSubscriptionId(u64);

impl PaneOutputSubscriptionId {
    /// Wraps a raw subscription identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw subscription identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Stable identifier for one pane-state event subscription on a live server
/// connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneStateSubscriptionId(u64);

impl PaneStateSubscriptionId {
    /// Wraps a raw subscription identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw subscription identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Opaque owner token for daemon-backed SDK waits.
///
/// The SDK assigns one owner token to each transport connection and then
/// allocates [`SdkWaitId`] values within that owner. The server treats the
/// owner as an opaque cancellation key; actual connection teardown cleanup is
/// still keyed by the server's private connection identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SdkWaitOwnerId(u64);

impl SdkWaitOwnerId {
    /// Wraps a raw SDK wait owner identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw SDK wait owner identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Stable identifier for one daemon-backed SDK wait under an
/// [`SdkWaitOwnerId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SdkWaitId(u64);

impl SdkWaitId {
    /// Wraps a raw SDK wait identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw SDK wait identifier.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}
