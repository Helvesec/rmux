//! A file-backed source invocation with an independent binary input channel.
use crate::{
    HandshakeRequest, CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1, CAPABILITY_SOURCE_FILE_INPUT_V1,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Capabilities used by this request; existing input DTOs retain their meaning.
pub const SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES: [&str; 3] = [
    CAPABILITY_SOURCE_FILE_INPUT_V1,
    CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
    CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1,
];

/// Runs one file-backed `source-file`. Its response is `SourceFileResponse`.
/// Source code is read from the named files; the payload is never source code.
/// With `requiring_queue_status`, completion uses `SourceFileQueueV1Response` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileInputV1Request {
    /// Mandatory per-request negotiation, checked before parsing or execution.
    pub handshake: HandshakeRequest,
    /// Original argv, including the command name or its user alias.
    pub arguments: Vec<String>,
    /// Working directory used by source paths and nested commands.
    pub caller_cwd: Option<PathBuf>,
    /// Finished binary input; empty EOF is distinct from an absent channel.
    pub stdin_payload: Option<Vec<u8>>,
}

impl SourceFileInputV1Request {
    /// Opts into an additive response carrying the outer source queue disposition.
    #[must_use]
    pub fn requiring_queue_status(mut self) -> Self {
        self.handshake
            .required_capabilities
            .push(crate::CAPABILITY_SOURCE_FILE_QUEUE_STATUS_V1.to_owned());
        self
    }

    /// Constructs an offer. Every response and transport error is terminal.
    #[must_use]
    pub fn new(
        arguments: Vec<String>,
        caller_cwd: Option<PathBuf>,
        stdin_payload: Option<Vec<u8>>,
    ) -> Self {
        Self {
            handshake: HandshakeRequest::requiring(SOURCE_FILE_INPUT_V1_REQUIRED_CAPABILITIES),
            arguments,
            caller_cwd,
            stdin_payload,
        }
    }
}

/// Negotiated source completion; the original SourceFileResponse wire shape is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileQueueV1Response {
    /// Stop the caller's queue when outer file selection fails with no matches.
    /// Errors reading selected files or inside sourced commands only affect status.
    pub stop_queue: bool,
    /// Original stdout, stderr and exit status, preserved without diagnostic parsing.
    pub source: crate::SourceFileResponse,
}
