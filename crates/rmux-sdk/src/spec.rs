//! Inert SDK command specification DTOs.
//!
//! The types in this module hold caller intent. They do not expose protocol
//! conversions, open IPC streams, start daemons, inspect
//! processes, probe endpoint paths, parse tmux command text, or resolve
//! targets.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{PaneRef, TerminalSizeSpec};
use crate::SessionName;

/// Explicit process command mode used by SDK builders and specs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ProcessCommandSpec {
    /// Execute a program directly with structured argv.
    ///
    /// On Windows, `.bat` and `.cmd` targets are rejected because `cmd.exe`
    /// cannot preserve arbitrary argv boundaries. Use [`Self::Shell`] when
    /// shell interpretation is intentional, or target a native executable.
    Argv(Vec<String>),
    /// Execute command text through the configured shell.
    Shell(String),
}

impl ProcessCommandSpec {
    pub(crate) fn into_proto(self) -> rmux_proto::ProcessCommand {
        match self {
            Self::Argv(argv) => rmux_proto::ProcessCommand::Argv(argv),
            Self::Shell(command) => rmux_proto::ProcessCommand::Shell(command),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Argv(argv) => argv.is_empty() || argv.first().is_some_and(String::is_empty),
            Self::Shell(command) => command.is_empty(),
        }
    }
}

/// Process-spawn fields shared by SDK command specs.
///
/// `process_command` is the explicit launch-mode path. The older `command`
/// field is retained for low-level tmux-compatible specs where a single item
/// means `$SHELL -c`; new SDK surfaces should use [`ProcessSpec::argv`],
/// [`ProcessSpec::shell`], or the matching session/pane builders rather than
/// struct literals.
#[derive(Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSpec {
    /// Legacy optional command vector. A single item keeps the historical
    /// `$SHELL -c` behavior in protocol handlers.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode.
    #[serde(default)]
    pub process_command: Option<ProcessCommandSpec>,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
}

impl ProcessSpec {
    /// Creates a process spec that executes direct argv.
    ///
    /// On Windows, `.bat` and `.cmd` targets are rejected; use a native
    /// executable for literal argv, or [`Self::shell`] when shell
    /// interpretation is intentional.
    #[must_use]
    pub fn argv<I, S>(command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            process_command: Some(ProcessCommandSpec::Argv(
                command.into_iter().map(Into::into).collect(),
            )),
            ..Self::default()
        }
    }

    /// Creates a process spec that executes command text through the shell.
    #[must_use]
    pub fn shell(command: impl Into<String>) -> Self {
        Self {
            process_command: Some(ProcessCommandSpec::Shell(command.into())),
            ..Self::default()
        }
    }

    pub(crate) fn into_proto_parts(
        self,
    ) -> (
        Option<Vec<String>>,
        Option<rmux_proto::ProcessCommand>,
        Option<Vec<String>>,
    ) {
        (
            self.command,
            self.process_command.map(ProcessCommandSpec::into_proto),
            self.environment,
        )
    }
}

impl fmt::Debug for ProcessSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessSpec")
            .field("command", &self.command)
            .field("process_command", &self.process_command)
            .finish_non_exhaustive()
    }
}

/// Reuse-related flags for `new-session` specs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSessionReuse {
    /// Attach to an existing target session instead of treating it as an error.
    #[serde(default)]
    pub attach_if_exists: bool,
    /// Detach other attached clients before attaching.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Detach and terminate other attached clients before attaching.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Skip client environment updates.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
}

/// Reuse-related flags for `attach-session` specs.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionReuse {
    /// Detach other attached clients before attaching.
    #[serde(default)]
    pub detach_other_clients: bool,
    /// Detach and terminate other attached clients before attaching.
    #[serde(default)]
    pub kill_other_clients: bool,
    /// Enable readonly attach mode.
    #[serde(default)]
    pub read_only: bool,
    /// Skip client environment updates.
    #[serde(default)]
    pub skip_environment_update: bool,
    /// Optional tmux client-flag names such as `read-only` or `active-pane`.
    #[serde(default)]
    pub flags: Option<Vec<String>>,
}

/// Terminal/runtime hints captured by a caller.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientTerminalSpec {
    /// Explicit terminal feature names contributed by top-level client flags.
    #[serde(default)]
    pub terminal_features: Vec<String>,
    /// Whether the invoking client should be treated as UTF-8 capable.
    #[serde(default)]
    pub utf8: bool,
}

/// SDK value object for `new-session`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSessionSpec {
    /// Optional exact session name to create.
    #[serde(default)]
    pub session_name: Option<SessionName>,
    /// Optional tmux format-expanded start directory for the new session.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Whether the session should remain detached after creation.
    #[serde(default)]
    pub detached: bool,
    /// Initial pane geometry, when explicitly requested.
    #[serde(default)]
    pub size: Option<TerminalSizeSpec>,
    /// Process-spawn fields for the initial pane.
    #[serde(default)]
    pub process: ProcessSpec,
    /// Optional target session or group name for grouped-session creation.
    #[serde(default)]
    pub group_target: Option<SessionName>,
    /// Reuse and client-detach flags.
    #[serde(default)]
    pub reuse: NewSessionReuse,
    /// Optional initial active-window name.
    #[serde(default)]
    pub window_name: Option<String>,
    /// Whether to print formatted session information.
    #[serde(default)]
    pub print_session_info: bool,
    /// Optional format template used when printing session information.
    #[serde(default)]
    pub print_format: Option<String>,
}

/// SDK value object for `attach-session`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachSessionSpec {
    /// Optional exact target session name.
    #[serde(default)]
    pub target: Option<SessionName>,
    /// Optional raw tmux-style target text, including window or pane selectors.
    #[serde(default)]
    pub target_spec: Option<String>,
    /// Reuse and client-detach flags.
    #[serde(default)]
    pub reuse: AttachSessionReuse,
    /// Optional tmux format-expanded working directory applied to the target.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Terminal/runtime hints captured from the invoking client.
    #[serde(default)]
    pub client_terminal: ClientTerminalSpec,
    /// The invoking client terminal size, when known.
    #[serde(default)]
    pub client_size: Option<TerminalSizeSpec>,
}

/// Reserved wire-v5 control-mode subscription fields for `refresh-client`.
///
/// RMUX 0.9 does not implement these fields. They remain inert values here;
/// the corresponding protocol fields are rejected by the server when populated.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionSpec {
    /// Reserved subscription-update field; unsupported by RMUX 0.9.
    #[serde(default)]
    pub subscriptions: Vec<String>,
    /// Reserved subscription-definition field; unsupported by RMUX 0.9.
    #[serde(default)]
    pub subscriptions_format: Vec<String>,
}

/// SDK value object for `refresh-client`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshClientSpec {
    /// Optional target-client identifier or `=`.
    #[serde(default)]
    pub target_client: Option<String>,
    /// Optional pan adjustment used with directional panning.
    #[serde(default)]
    pub adjustment: Option<u32>,
    /// Whether client panning should be cleared.
    #[serde(default)]
    pub clear_pan: bool,
    /// Whether the client view should pan left.
    #[serde(default)]
    pub pan_left: bool,
    /// Whether the client view should pan right.
    #[serde(default)]
    pub pan_right: bool,
    /// Whether the client view should pan up.
    #[serde(default)]
    pub pan_up: bool,
    /// Whether the client view should pan down.
    #[serde(default)]
    pub pan_down: bool,
    /// Whether only the status line should be redrawn.
    #[serde(default)]
    pub status_only: bool,
    /// Whether the client clipboard should be queried.
    #[serde(default)]
    pub clipboard_query: bool,
    /// Optional client-flag string from `-f`.
    #[serde(default)]
    pub flags: Option<String>,
    /// Optional client-flag string from `-F`.
    #[serde(default)]
    pub flags_alias: Option<String>,
    /// Reserved control-mode subscription fields; unsupported by RMUX 0.9.
    #[serde(default)]
    pub subscriptions: SubscriptionSpec,
    /// Optional control-mode size string from `-C`.
    #[serde(default)]
    pub control_size: Option<String>,
    /// Reserved wire-v5 colour-report field; unsupported by RMUX 0.9.
    #[serde(default)]
    pub colour_report: Option<String>,
}

/// Low-level split orientation used by [`SplitSpec`] and command DTOs.
///
/// Variants follow tmux's flag convention (pane arrangement). Prefer the
/// ergonomic [`SplitDirection`](crate::SplitDirection) (`Right`/`Left`/`Up`/
/// `Down`) on [`Pane::split`](crate::Pane::split), which removes the
/// arrangement-vs-divider-line ambiguity.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SplitDirectionSpec {
    /// Stacked panes (top + bottom). Matches tmux `split-window -v`,
    /// the tmux default when no flag is passed.
    #[default]
    Vertical,
    /// Side-by-side panes (left + right). Matches tmux `split-window -h`.
    Horizontal,
}

/// SDK split target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SplitTargetSpec {
    /// Split the active pane in the addressed session.
    Session(SessionName),
    /// Split the addressed pane directly.
    Pane(PaneRef),
}

/// SDK value object for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitSpec {
    /// Exact split target.
    pub target: SplitTargetSpec,
    /// Requested split direction.
    #[serde(default)]
    pub direction: SplitDirectionSpec,
    /// Whether the new pane is inserted before the target on the chosen
    /// axis (tmux `-b`). Default `false` puts the new pane after the target.
    #[serde(default)]
    pub before: bool,
    /// Process-spawn fields for the new pane.
    #[serde(default)]
    pub process: ProcessSpec,
}

#[cfg(test)]
mod request_tests;
