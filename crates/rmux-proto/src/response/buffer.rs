//! Paste-buffer command responses.

use serde::{Deserialize, Serialize};

use super::CommandOutput;

/// Response payload for `set-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetBufferResponse {
    /// The name of the created or replaced buffer.
    pub buffer_name: String,
}

/// Response payload for `show-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowBufferResponse {
    /// The pre-rendered stdout bytes for the CLI.
    pub output: CommandOutput,
}

impl ShowBufferResponse {
    /// Returns the reusable stdout payload.
    #[must_use]
    pub fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}

/// Response payload for `paste-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasteBufferResponse {
    /// The buffer name that was pasted.
    pub buffer_name: String,
}

/// Response payload for `list-buffers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListBuffersResponse {
    /// The pre-rendered stdout bytes for the CLI.
    pub output: CommandOutput,
}

impl ListBuffersResponse {
    /// Returns the reusable stdout payload.
    #[must_use]
    pub fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}

/// Response payload for `delete-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteBufferResponse {
    /// The name of the deleted buffer.
    pub buffer_name: String,
}

/// Response payload for `load-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadBufferResponse {
    /// The name of the created or replaced buffer.
    pub buffer_name: String,
}

/// Response payload for `save-buffer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveBufferResponse {
    /// The name of the saved buffer.
    pub buffer_name: String,
}
