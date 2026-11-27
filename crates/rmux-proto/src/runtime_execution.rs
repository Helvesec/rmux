//! Atomic capability negotiation and argv execution for detached CLI commands.
//!
//! This is an additive v8 offer, encoded by the normal typed frame codec. A
//! supporting peer checks the embedded requirements before parsing or effects.
//! Historical peers reject its unknown tag in the frame decoder, before request
//! hooks, and close the connection. Only that precise rejection permits a new
//! legacy connection; ambiguous transport errors never permit replay.

use crate::{HandshakeRequest, RmuxError, CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// An execution offer, with explicit capability requirements checked first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCommandExecutionRequest {
    /// Required wire window and capabilities, validated before command parsing.
    pub handshake: HandshakeRequest,
    /// Original argv, interpreted only by the authoritative server parser.
    pub arguments: Vec<String>,
    /// Invoking client's working directory for command launch semantics.
    pub caller_cwd: Option<PathBuf>,
}

impl RuntimeCommandExecutionRequest {
    /// Requires floating creation without changing the historical wire shape.
    #[must_use]
    pub fn requiring_floating(mut self) -> Self {
        self.handshake
            .required_capabilities
            .push(crate::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1.to_owned());
        self
    }

    /// Requires absolute floating-pane translation without changing the wire shape.
    #[must_use]
    pub fn requiring_resize_pane_position(mut self) -> Self {
        self.handshake
            .required_capabilities
            .push(crate::CAPABILITY_RESIZE_PANE_POSITION_V1.to_owned());
        self
    }

    /// Requires existing-pane placement conversion without changing the wire shape.
    #[must_use]
    pub fn requiring_set_pane_placement(mut self) -> Self {
        self.handshake
            .required_capabilities
            .push(crate::CAPABILITY_SET_PANE_PLACEMENT_V1.to_owned());
        self
    }

    /// Offers the current execution contract for these original arguments.
    #[must_use]
    pub fn new(arguments: Vec<String>, caller_cwd: Option<PathBuf>) -> Self {
        Self {
            handshake: HandshakeRequest::requiring([CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION]),
            arguments,
            caller_cwd,
        }
    }
}

/// Explicit result of capability negotiation and, when admitted, execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeCommandExecutionResponse {
    /// No effects occurred; retain the CLI's helper validation/transition path.
    Fallback,
    /// Negotiation failed before parsing or effects, without command-error hooks.
    Unsupported(RmuxError),
    /// Execution finished; the client must never replay this command.
    Complete {
        /// Canonical command name for the existing CLI error formatter.
        command_name: String,
        /// Command stdout, preserved byte for byte by the standard frame codec.
        stdout: Vec<u8>,
        /// Execution error, if any, using the existing typed error contract.
        error: Option<RmuxError>,
        /// Process exit status, including a nested command's explicit status.
        exit_status: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_offer_is_additive_to_the_existing_execution_envelope() {
        let request = RuntimeCommandExecutionRequest::new(
            vec!["set-pane-placement".to_owned(), "floating".to_owned()],
            None,
        )
        .requiring_set_pane_placement();
        assert_eq!(
            request.handshake.required_capabilities,
            [
                crate::CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION,
                crate::CAPABILITY_SET_PANE_PLACEMENT_V1,
            ]
        );
        assert!(crate::SUPPORTED_CAPABILITIES.contains(&crate::CAPABILITY_SET_PANE_PLACEMENT_V1));
    }
}
