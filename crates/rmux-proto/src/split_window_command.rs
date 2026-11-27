//! Explicit atomic split/key command admission. Historical boolean DTOs are unchanged.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{HandshakeRequest, RmuxError, CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1};

/// A single split command and its separate, uninterpreted terminal payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowCommandV1Request {
    /// Mandatory capability and exact-envelope compatibility requirements.
    pub handshake: HandshakeRequest,
    /// Lossless split argv, canonicalized and validated before execution.
    pub arguments: Vec<String>,
    /// Invoking client's working directory.
    pub caller_cwd: Option<PathBuf>,
    /// Binary output for `-I` without a command; Some(empty) preserves EOF.
    pub stdin_payload: Option<Vec<u8>>,
}

impl SplitWindowCommandV1Request {
    /// Constructs a versioned offer. A failure must never trigger a legacy retry.
    #[must_use]
    pub fn new(
        arguments: Vec<String>,
        caller_cwd: Option<PathBuf>,
        stdin_payload: Option<Vec<u8>>,
    ) -> Self {
        Self {
            handshake: HandshakeRequest::requiring([CAPABILITY_SPLIT_WINDOW_KEY_INPUT_V1]),
            arguments,
            caller_cwd,
            stdin_payload,
        }
    }
}

/// Terminal result of an offer; neither variant permits automatic replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitWindowCommandV1Response {
    /// Rejected before execution, hooks, options, spawn or terminal injection.
    NotAdmitted {
        /// Capability or command-shape rejection.
        error: RmuxError,
    },
    /// Admitted execution finished, possibly with an error after effects.
    Complete {
        /// Byte-exact command output.
        stdout: Vec<u8>,
        /// Ordinary command error, when present.
        error: Option<RmuxError>,
        /// Final command exit status.
        exit_status: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sc2_complete_offer_including_binary_payload_is_frame_bounded() {
        let request =
            crate::Request::SplitWindowCommandV1(Box::new(SplitWindowCommandV1Request::new(
                vec!["split-window".into(), "-Ik".into()],
                None,
                Some(vec![0; crate::DEFAULT_MAX_DETACHED_FRAME_LENGTH]),
            )));
        assert!(matches!(
            crate::encode_frame(&request),
            Err(RmuxError::FrameTooLarge { .. })
        ));
    }
}
