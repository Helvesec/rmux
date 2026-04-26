//! Shared protocol error types.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Shared protocol and validation errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
pub enum RmuxError {
    /// A session name was empty.
    #[error("invalid session name: session names must be non-empty")]
    EmptySessionName,
    /// A session name contained `:` or `.`.
    #[error("invalid session name: session names must not contain ':' or '.'")]
    InvalidSessionNameCharacter,
    /// A target string failed exact detached parsing.
    #[error("invalid target '{value}': {reason}")]
    InvalidTarget {
        /// The original target string.
        value: String,
        /// The validation failure reason.
        reason: String,
    },
    /// An unsupported public command name was requested.
    #[error("unknown command: {0}")]
    UnknownCommand(String),
    /// A session name collided with an existing session.
    #[error("duplicate session: {0}")]
    DuplicateSession(String),
    /// A session target did not resolve.
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// The server encountered an unexpected runtime failure.
    #[error("server error: {0}")]
    Server(String),
    /// A command surfaced an exact tmux-compatible user-facing error.
    #[error("{0}")]
    Message(String),
    /// A `set-option` request violated the supported contract.
    #[error("invalid set-option request: {0}")]
    InvalidSetOption(String),
    /// A codec frame claimed a payload larger than the configured maximum.
    #[error("codec frame length {length} exceeds maximum {maximum}")]
    FrameTooLarge {
        /// The announced frame payload length.
        length: usize,
        /// The configured maximum payload length.
        maximum: usize,
    },
    /// A codec frame announced an empty payload.
    #[error("codec frame payloads must not be empty")]
    EmptyFrame,
    /// A frame did not start with the RMUX frame magic byte.
    #[error("bad RMUX frame magic: expected 0x52, got 0x{0:02x}")]
    BadFrameMagic(u8),
    /// A frame used a wire version this build does not support.
    #[error("unsupported RMUX wire version {got}; supported range is {minimum}..={maximum}")]
    UnsupportedWireVersion {
        /// The wire version advertised by the frame.
        got: u32,
        /// The minimum supported wire version.
        minimum: u32,
        /// The maximum supported wire version.
        maximum: u32,
    },
    /// A full frame was requested but the buffer stopped early.
    #[error("incomplete frame: expected {expected} payload bytes, received {received}")]
    IncompleteFrame {
        /// The payload bytes expected by the frame header.
        expected: usize,
        /// The payload bytes actually available.
        received: usize,
    },
    /// Bincode encoding failed.
    #[error("failed to encode frame payload: {0}")]
    Encode(String),
    /// Bincode decoding failed.
    #[error("failed to decode frame payload: {0}")]
    Decode(String),
}

impl RmuxError {
    /// Creates an invalid-target error with a stable message shape.
    #[must_use]
    pub fn invalid_target(value: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidTarget {
            value: value.into(),
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RmuxError;

    #[test]
    fn display_messages_are_stable() {
        assert_eq!(
            RmuxError::EmptySessionName.to_string(),
            "invalid session name: session names must be non-empty"
        );
        assert_eq!(
            RmuxError::InvalidSessionNameCharacter.to_string(),
            "invalid session name: session names must not contain ':' or '.'"
        );
        assert_eq!(
            RmuxError::invalid_target("alpha:1", "only window index 0 is supported").to_string(),
            "invalid target 'alpha:1': only window index 0 is supported"
        );
        assert_eq!(
            RmuxError::UnknownCommand("list-sessions".to_owned()).to_string(),
            "unknown command: list-sessions"
        );
        assert_eq!(
            RmuxError::DuplicateSession("Alpha".to_owned()).to_string(),
            "duplicate session: Alpha"
        );
        assert_eq!(
            RmuxError::SessionNotFound("Alpha".to_owned()).to_string(),
            "session not found: Alpha"
        );
        assert_eq!(
            RmuxError::Server("pty resize failed".to_owned()).to_string(),
            "server error: pty resize failed"
        );
        assert_eq!(
            RmuxError::Message("window only linked to one session".to_owned()).to_string(),
            "window only linked to one session"
        );
        assert_eq!(
            RmuxError::InvalidSetOption("status is not an array option".to_owned()).to_string(),
            "invalid set-option request: status is not an array option"
        );
        assert_eq!(
            RmuxError::FrameTooLarge {
                length: 2_000_000,
                maximum: 1_048_576
            }
            .to_string(),
            "codec frame length 2000000 exceeds maximum 1048576"
        );
        assert_eq!(
            RmuxError::EmptyFrame.to_string(),
            "codec frame payloads must not be empty"
        );
        assert_eq!(
            RmuxError::BadFrameMagic(0).to_string(),
            "bad RMUX frame magic: expected 0x52, got 0x00"
        );
        assert_eq!(
            RmuxError::UnsupportedWireVersion {
                got: 2,
                minimum: 1,
                maximum: 1,
            }
            .to_string(),
            "unsupported RMUX wire version 2; supported range is 1..=1"
        );
        assert_eq!(
            RmuxError::IncompleteFrame {
                expected: 100,
                received: 50
            }
            .to_string(),
            "incomplete frame: expected 100 payload bytes, received 50"
        );
        assert_eq!(
            RmuxError::Encode("bincode error".to_owned()).to_string(),
            "failed to encode frame payload: bincode error"
        );
        assert_eq!(
            RmuxError::Decode("bincode error".to_owned()).to_string(),
            "failed to decode frame payload: bincode error"
        );
    }
}
