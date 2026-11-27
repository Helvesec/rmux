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

/// Runs one file-backed `source-file`. Default completion uses `SourceFileResponse`.
/// An on-demand offer may receive `SourceFileReadStdinV1Response` before completion.
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

    /// Offers an on-demand channel without including any bytes in the initial request.
    #[must_use]
    pub fn on_demand(arguments: Vec<String>, caller_cwd: Option<PathBuf>) -> Self {
        let mut offer = Self::new(arguments, caller_cwd, None);
        offer
            .handshake
            .required_capabilities
            .push(crate::CAPABILITY_SOURCE_FILE_INPUT_PULL_V1.into());
        offer
    }

    /// Whether this offer requires a same-connection stdin exchange.
    pub fn requests_input_pull(&self) -> bool {
        self.handshake
            .requires(crate::CAPABILITY_SOURCE_FILE_INPUT_PULL_V1)
    }

    /// Whether a second authenticated connection may own this invocation's stdin.
    pub fn requests_input_relay(&self) -> bool {
        self.handshake
            .requires(crate::CAPABILITY_SOURCE_FILE_INPUT_RELAY_V1)
    }

    /// Constructs a finished-input offer. Its first response is terminal.
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

/// One stdin read requested by the currently executing file-backed source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileReadStdinV1Response;

/// The caller's reply to a stdin read on the same connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileInputDataV1Request {
    /// Finished binary bytes (including empty EOF), or the caller's read error.
    pub data: Result<Vec<u8>, String>,
}

impl SourceFileInputDataV1Request {
    /// Maximum byte payload after the request tag, Result tag and Vec length.
    pub const MAX_BYTES: usize =
        crate::DEFAULT_MAX_DETACHED_FRAME_LENGTH - 2 * size_of::<u32>() - size_of::<u64>();
}

/// One-time credentials for a stdin reader. Only the ticket goes into its claim.
/// The daemon proves it owns this offer before the reader touches stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileRelayV1Response {
    /// Random, single-use claim bound to the source caller's user identity.
    pub ticket: [u8; 16],
    /// Independent challenge for the daemon on the reader connection.
    pub proof: [u8; 16],
}

/// Claim a pending reader on a connection authenticated as the source caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileRelayV1Request {
    /// One-time ticket received on the caller connection.
    pub ticket: [u8; 16],
}

/// Confirm the claim before the reader waits for a stdin demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileRelayReadyV1Response {
    /// Proof received separately by the source caller.
    pub proof: [u8; 16],
}
