//! `new-pane -I`: a finished tool's output, offered once with the command.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{
    HandshakeRequest, RmuxError, CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1, CAPABILITY_NEW_PANE_INPUT_V1,
};

/// What a peer must support before this offer may be sent or admitted.
///
/// The three are named because the offer uses all three and none of them
/// implies another: it executes a command the way the runtime execution offer
/// does, it creates an explicit floating pane, and it carries the finished
/// input this capability is about.
pub const NEW_PANE_INPUT_V1_REQUIRED_CAPABILITIES: [&str; 3] = [
    CAPABILITY_NEW_PANE_INPUT_V1,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1,
];

/// One `new-pane` and the finished bytes it is to display, kept apart from it.
///
/// The payload is data and never command text: nothing in it is parsed, sourced
/// or expanded. `Some(empty)` is a channel that reached end of file without a
/// byte, which is a different thing from `None`, where there is no channel at
/// all and `-I` has nothing to read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPaneInputV1Request {
    /// Mandatory capability and exact-envelope compatibility requirements.
    pub handshake: HandshakeRequest,
    /// Lossless `new-pane` argv, resolved by the server's own parser once.
    pub arguments: Vec<String>,
    /// Invoking client's working directory.
    pub caller_cwd: Option<PathBuf>,
    /// The finished output read from the client's stdin; `Some(empty)` is EOF.
    pub stdin_payload: Option<Vec<u8>>,
}

impl NewPaneInputV1Request {
    /// Constructs the offer. A failure must never trigger a retry or a fallback.
    #[must_use]
    pub fn new(
        arguments: Vec<String>,
        caller_cwd: Option<PathBuf>,
        stdin_payload: Option<Vec<u8>>,
    ) -> Self {
        Self {
            handshake: HandshakeRequest::requiring(NEW_PANE_INPUT_V1_REQUIRED_CAPABILITIES),
            arguments,
            caller_cwd,
            stdin_payload,
        }
    }
}

/// Terminal result of the offer; neither variant permits a second execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NewPaneInputV1Response {
    /// Rejected before parsing, admission, creation or any injection.
    NotAdmitted {
        /// Capability or command-shape rejection.
        error: RmuxError,
    },
    /// Admitted execution finished, possibly with an error after effects.
    Complete {
        /// Byte-exact command output, which `-P` fills.
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
    fn the_whole_offer_is_bounded_by_the_detached_frame_limit_not_the_payload_alone() {
        // The envelope carries argv, the cwd and the handshake beside the
        // bytes, so a payload just under the limit still cannot be framed.
        let request = crate::Request::NewPaneInputV1(Box::new(NewPaneInputV1Request::new(
            vec!["new-pane".into(), "-I".into()],
            None,
            Some(vec![0; crate::DEFAULT_MAX_DETACHED_FRAME_LENGTH]),
        )));
        assert!(matches!(
            crate::encode_frame(&request),
            Err(RmuxError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn an_eof_without_a_byte_is_not_the_absence_of_a_channel() {
        let eof = NewPaneInputV1Request::new(vec!["new-pane".into()], None, Some(Vec::new()));
        let none = NewPaneInputV1Request::new(vec!["new-pane".into()], None, None);
        assert_ne!(eof, none);
        assert_eq!(eof.stdin_payload.as_deref(), Some(&[][..]));
        assert_eq!(none.stdin_payload, None);
    }
}
