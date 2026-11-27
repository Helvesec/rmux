//! Request payloads that evaluate server-local shell commands: `run-shell` and `if-shell`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{PaneTarget, Target};

/// Request payload for `run-shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunShellRequest {
    /// The server-local shell command passed to `sh -c`.
    pub command: String,
    /// Literal positional format arguments exposed as `#{1}`, `#{2}`, and so on.
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Whether the command should run fire-and-forget without output capture.
    pub background: bool,
    /// Whether the command should be executed as tmux commands instead of `sh -c`.
    #[serde(default)]
    pub as_commands: bool,
    /// Whether stderr should be captured alongside stdout.
    #[serde(default)]
    pub show_stderr: bool,
    /// Optional delay, in seconds, before the command runs.
    #[serde(default)]
    pub delay_seconds: Option<RunShellDelaySeconds>,
    /// Optional explicit working directory.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional explicit target pane used for format and session context.
    #[serde(default)]
    pub target: Option<PaneTarget>,
    /// Internal source-file recursion depth inherited by queued run-shell.
    #[serde(default)]
    pub source_depth: Option<usize>,
}

/// Losslessly serializable `run-shell -d` seconds value with stable `Eq`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RunShellDelaySeconds(pub f64);

impl RunShellDelaySeconds {
    /// Returns the raw seconds value.
    #[must_use]
    pub const fn as_secs_f64(self) -> f64 {
        self.0
    }
}

impl PartialEq for RunShellDelaySeconds {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}

impl Eq for RunShellDelaySeconds {}

/// Request payload for `if-shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfShellRequest {
    /// The condition string expanded through the shared formatter before evaluation.
    pub condition: String,
    /// Whether to evaluate the expanded condition with format truthiness instead of `sh -c`.
    pub format_mode: bool,
    /// The nested RMUX command string dispatched when the condition is true.
    pub then_command: String,
    /// The optional nested RMUX command string dispatched when the condition is false.
    pub else_command: Option<String>,
    /// Optional exact target used as shared-format context.
    pub target: Option<Target>,
    /// The caller working directory used to resolve nested relative file paths.
    pub caller_cwd: Option<PathBuf>,
    /// Whether the condition should be evaluated asynchronously.
    #[serde(default)]
    pub background: bool,
}
