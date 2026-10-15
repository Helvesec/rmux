//! Terminal-local Program Status Protocol (OSC 7501) records.
//!
//! These are advisory reports from a program controlling a PTY, not process
//! identity or authorization. Text is plain, single-line UTF-8, never markup.
use serde::{Deserialize, Serialize};

/// One of the five stored OSC 7501 states (`clear` removes records).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramStatusState {
    /// At rest, waiting for the next instruction.
    Idle,
    /// Work is in progress.
    Working,
    /// Work finished and its result is ready to view.
    Done,
    /// User intervention is required.
    Blocked,
    /// Work failed and stopped.
    Error,
}

impl ProgramStatusState {
    /// Stable protocol spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::Error => "error",
        }
    }

    /// Whether the record survives process exit and a new shell prompt.
    #[must_use]
    pub const fn is_result(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }
}

/// A bounded, validated OSC 7501 record. An empty `id` denotes the root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProgramStatusRecord {
    /// Hierarchical terminal-local identity, or an empty string for the root.
    pub id: String,
    /// Current reported state.
    pub state: ProgramStatusState,
    /// Explicit application identifier. A missing value inherits from ancestors.
    pub app: Option<String>,
    /// `permission`, `question`, or `auth`, only for a blocked record.
    pub kind: Option<String>,
    /// Percentage from 0 to 100, only for working/blocked records.
    pub progress: Option<u8>,
    /// Decoded plain-text label, at most 192 UTF-8 bytes.
    pub title: Option<String>,
    /// Decoded plain-text description, at most 2048 UTF-8 bytes.
    pub message: Option<String>,
}
