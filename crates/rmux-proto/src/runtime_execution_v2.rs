//! Atomic runtime offer with support requirements separate from operation intent.
//!
//! This additive wire-9 request leaves C2S131 unchanged. An older peer rejects
//! its unknown tag before dispatch. Callers must not downgrade or replay it.
//! Replies use the existing `RuntimeCommandExecutionResponse`: admission errors
//! are `Unsupported` before effects; every result after execution is `Complete`.

use crate::{HandshakeRequest, CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The operation declared before the server resolves the original argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeCommandIntent {
    /// Ordinary admitted CLI commands, excluding the three protected operations.
    GenericCli,
    /// Exactly one floating-pane creation.
    NewFloatingPane,
    /// Exactly one floating-pane translation (`resize-pane -X/-Y`).
    MoveFloatingPane,
    /// Exactly one existing-pane placement conversion.
    SetPanePlacement,
}

impl RuntimeCommandIntent {
    /// Existing support requirement for this operation, in addition to runtime.
    #[must_use]
    pub const fn required_capability(self) -> Option<&'static str> {
        match self {
            Self::GenericCli => None,
            Self::NewFloatingPane => Some(crate::CAPABILITY_NEW_PANE_FLOATING_EXPLICIT_V1),
            Self::MoveFloatingPane => Some(crate::CAPABILITY_RESIZE_PANE_POSITION_V1),
            Self::SetPanePlacement => Some(crate::CAPABILITY_SET_PANE_PLACEMENT_V1),
        }
    }
}

/// One request carrying both negotiation and an independent operation intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeCommandExecutionV2Request {
    /// Support requirements only; validated before parsing or effects.
    pub handshake: HandshakeRequest,
    /// Original argv, resolved by the authoritative server parser exactly once.
    pub arguments: Vec<String>,
    /// Invoking client's working directory.
    pub caller_cwd: Option<PathBuf>,
    /// Declaration to compare against the resolved command, never inferred from it.
    pub intent: RuntimeCommandIntent,
}

impl RuntimeCommandExecutionV2Request {
    /// Offers an operation with its minimal support requirements.
    #[must_use]
    pub fn new(
        arguments: Vec<String>,
        caller_cwd: Option<PathBuf>,
        intent: RuntimeCommandIntent,
    ) -> Self {
        Self {
            handshake: HandshakeRequest::requiring(
                std::iter::once(CAPABILITY_CLI_RUNTIME_COMMAND_EXECUTION)
                    .chain(intent.required_capability()),
            ),
            arguments,
            caller_cwd,
            intent,
        }
    }
}
