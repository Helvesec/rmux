//! Window snapshot export and restore responses.

use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::{RmuxWindowSnapshot, WindowId, WindowTarget};

/// Response payload for `export-window-snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportWindowSnapshotResponse {
    /// The typed snapshot captured from the live window.
    pub snapshot: RmuxWindowSnapshot,
    /// Compact JSON encoding of `snapshot`, terminated by one newline.
    pub output: CommandOutput,
}

impl ExportWindowSnapshotResponse {
    /// Creates an export response and its reusable stdout payload.
    ///
    /// # Errors
    ///
    /// Returns the JSON serialization error if the typed snapshot cannot be
    /// encoded for CLI output.
    pub fn from_snapshot(snapshot: RmuxWindowSnapshot) -> Result<Self, serde_json::Error> {
        let mut stdout = serde_json::to_vec(&snapshot)?;
        stdout.push(b'\n');
        Ok(Self {
            snapshot,
            output: CommandOutput::from_stdout(stdout),
        })
    }

    /// Returns the reusable stdout payload.
    #[must_use]
    pub const fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}

/// Response payload for `restore-window-snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreWindowSnapshotResponse {
    /// The restored target occurrence.
    pub target: WindowTarget,
    /// Stable canonical window identity restored by the request.
    pub window_id: WindowId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Explicit V2 canonical snapshot and its reusable JSON output.
pub struct ExportWindowSnapshotV2Response {
    /// The typed snapshot captured from the live window.
    pub snapshot: crate::RmuxWindowSnapshotV2,
    /// Compact JSON encoding of `snapshot`, terminated by one newline.
    pub output: CommandOutput,
}

impl ExportWindowSnapshotV2Response {
    /// Creates an export response and its reusable stdout payload.
    ///
    /// # Errors
    ///
    /// Returns the JSON serialization error if the typed snapshot cannot be
    /// encoded for CLI output.
    pub fn from_snapshot(snapshot: crate::RmuxWindowSnapshotV2) -> Result<Self, serde_json::Error> {
        let mut stdout = serde_json::to_vec(&snapshot)?;
        stdout.push(b'\n');
        Ok(Self {
            snapshot,
            output: CommandOutput::from_stdout(stdout),
        })
    }

    /// Returns the reusable stdout payload.
    #[must_use]
    pub const fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}

/// Response payload for `restore-window-snapshot`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreWindowSnapshotV2Response {
    /// The restored target occurrence.
    pub target: WindowTarget,
    /// Stable canonical window identity restored by the request.
    pub window_id: WindowId,
}
