use std::error::Error;
use std::fmt;

/// An SDK-owned protocol diagnostic.
///
/// Display and Debug retain the diagnostic, while the transport representation
/// stays private. Use the field accessors for SDK decisions; matching or
/// downcasting to `rmux-proto` errors is no longer a supported public API.
/// Like [`super::RmuxError`], this type deliberately does not implement `Clone`.
pub struct ProtocolError {
    pub(super) inner: rmux_proto::RmuxError,
}

impl ProtocolError {
    /// Creates a protocol diagnostic from an SDK caller's message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            inner: rmux_proto::RmuxError::Message(message.into()),
        }
    }

    /// Returns the colliding session name for a duplicate-session diagnostic.
    #[must_use]
    pub fn duplicate_session(&self) -> Option<&str> {
        match &self.inner {
            rmux_proto::RmuxError::DuplicateSession(name) => Some(name),
            _ => None,
        }
    }

    /// Returns the missing session name, distinct from an invalid target.
    #[must_use]
    pub fn session_not_found(&self) -> Option<&str> {
        match &self.inner {
            rmux_proto::RmuxError::SessionNotFound(name) => Some(name),
            _ => None,
        }
    }

    /// Returns the invalid target's original value and validation reason.
    #[must_use]
    pub fn invalid_target(&self) -> Option<(&str, &str)> {
        match &self.inner {
            rmux_proto::RmuxError::InvalidTarget { value, reason } => Some((value, reason)),
            _ => None,
        }
    }

    /// Returns the server failure message without its display prefix.
    /// A plain message diagnostic does not become a server failure.
    #[must_use]
    pub fn server_message(&self) -> Option<&str> {
        match &self.inner {
            rmux_proto::RmuxError::Server(message) => Some(message),
            _ => None,
        }
    }

    pub(crate) fn into_proto(self) -> rmux_proto::RmuxError {
        self.inner
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.inner, formatter)
    }
}

impl fmt::Debug for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, formatter)
    }
}

// The original protocol diagnostic was a leaf. Keep a meaningful SDK leaf,
// without exposing the private transport value through dynamic downcasting.
impl Error for ProtocolError {}
