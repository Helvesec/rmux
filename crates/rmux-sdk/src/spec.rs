//! Inert SDK command specification DTOs.
//!
//! The types in this module hold caller intent and map to `rmux-proto`
//! request payloads. They do not open IPC streams, start daemons, inspect
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
    Argv(Vec<String>),
    /// Execute command text through the configured shell.
    Shell(String),
}

impl From<ProcessCommandSpec> for rmux_proto::ProcessCommand {
    fn from(value: ProcessCommandSpec) -> Self {
        match value {
            ProcessCommandSpec::Argv(argv) => Self::Argv(argv),
            ProcessCommandSpec::Shell(command) => Self::Shell(command),
        }
    }
}

impl ProcessCommandSpec {
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
            self.process_command.map(rmux_proto::ProcessCommand::from),
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

impl From<ClientTerminalSpec> for rmux_proto::ClientTerminalContext {
    fn from(value: ClientTerminalSpec) -> Self {
        Self {
            terminal_features: value.terminal_features,
            utf8: value.utf8,
        }
    }
}

impl From<rmux_proto::ClientTerminalContext> for ClientTerminalSpec {
    fn from(value: rmux_proto::ClientTerminalContext) -> Self {
        Self {
            terminal_features: value.terminal_features,
            utf8: value.utf8,
        }
    }
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

impl From<NewSessionSpec> for rmux_proto::NewSessionExtRequest {
    fn from(value: NewSessionSpec) -> Self {
        let (command, process_command, environment) = value.process.into_proto_parts();
        Self {
            session_name: value.session_name,
            working_directory: value.working_directory,
            detached: value.detached,
            size: value.size.map(Into::into),
            environment,
            group_target: value.group_target,
            attach_if_exists: value.reuse.attach_if_exists,
            detach_other_clients: value.reuse.detach_other_clients,
            kill_other_clients: value.reuse.kill_other_clients,
            flags: value.reuse.flags,
            window_name: value.window_name,
            print_session_info: value.print_session_info,
            print_format: value.print_format,
            command,
            process_command,
            passthrough: false,
        }
    }
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

impl From<AttachSessionSpec> for rmux_proto::AttachSessionExt2Request {
    fn from(value: AttachSessionSpec) -> Self {
        Self {
            target: value.target,
            target_spec: value.target_spec,
            detach_other_clients: value.reuse.detach_other_clients,
            kill_other_clients: value.reuse.kill_other_clients,
            read_only: value.reuse.read_only,
            skip_environment_update: value.reuse.skip_environment_update,
            flags: value.reuse.flags,
            working_directory: value.working_directory,
            client_terminal: value.client_terminal.into(),
            client_size: value.client_size.map(Into::into),
        }
    }
}

/// Control-mode subscription fields for `refresh-client`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionSpec {
    /// Control-mode subscription updates from `refresh-client -A`.
    #[serde(default)]
    pub subscriptions: Vec<String>,
    /// Control-mode subscription definitions from `refresh-client -B`.
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
    /// Control-mode subscription fields.
    #[serde(default)]
    pub subscriptions: SubscriptionSpec,
    /// Optional control-mode size string from `-C`.
    #[serde(default)]
    pub control_size: Option<String>,
    /// Optional control-mode colour report request from `-r`.
    #[serde(default)]
    pub colour_report: Option<String>,
}

impl From<RefreshClientSpec> for rmux_proto::RefreshClientRequest {
    fn from(value: RefreshClientSpec) -> Self {
        Self {
            target_client: value.target_client,
            adjustment: value.adjustment,
            clear_pan: value.clear_pan,
            pan_left: value.pan_left,
            pan_right: value.pan_right,
            pan_up: value.pan_up,
            pan_down: value.pan_down,
            status_only: value.status_only,
            clipboard_query: value.clipboard_query,
            flags: value.flags,
            flags_alias: value.flags_alias,
            subscriptions: value.subscriptions.subscriptions,
            subscriptions_format: value.subscriptions.subscriptions_format,
            control_size: value.control_size,
            colour_report: value.colour_report,
        }
    }
}

/// Low-level split orientation used by [`SplitSpec`] and the script-level
/// `RmuxCommand` API.
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

impl From<SplitDirectionSpec> for rmux_proto::SplitDirection {
    fn from(value: SplitDirectionSpec) -> Self {
        match value {
            SplitDirectionSpec::Vertical => Self::Vertical,
            SplitDirectionSpec::Horizontal => Self::Horizontal,
        }
    }
}

impl From<rmux_proto::SplitDirection> for SplitDirectionSpec {
    fn from(value: rmux_proto::SplitDirection) -> Self {
        match value {
            rmux_proto::SplitDirection::Vertical => Self::Vertical,
            rmux_proto::SplitDirection::Horizontal => Self::Horizontal,
        }
    }
}

/// SDK split target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SplitTargetSpec {
    /// Split the active pane in the addressed session.
    Session(SessionName),
    /// Split the addressed pane directly.
    Pane(PaneRef),
}

impl From<SplitTargetSpec> for rmux_proto::SplitWindowTarget {
    fn from(value: SplitTargetSpec) -> Self {
        match value {
            SplitTargetSpec::Session(session_name) => Self::Session(session_name),
            SplitTargetSpec::Pane(target) => Self::Pane(target.into()),
        }
    }
}

impl From<rmux_proto::SplitWindowTarget> for SplitTargetSpec {
    fn from(value: rmux_proto::SplitWindowTarget) -> Self {
        match value {
            rmux_proto::SplitWindowTarget::Session(session_name) => Self::Session(session_name),
            rmux_proto::SplitWindowTarget::Pane(target) => Self::Pane(target.into()),
        }
    }
}

impl From<&SplitTargetSpec> for rmux_proto::SplitWindowTarget {
    fn from(value: &SplitTargetSpec) -> Self {
        match value {
            SplitTargetSpec::Session(session_name) => Self::Session(session_name.clone()),
            SplitTargetSpec::Pane(target) => Self::Pane(target.into()),
        }
    }
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

impl From<SplitSpec> for rmux_proto::SplitWindowExtRequest {
    fn from(value: SplitSpec) -> Self {
        let (command, process_command, environment) = value.process.into_proto_parts();
        Self {
            target: value.target.into(),
            direction: value.direction.into(),
            before: value.before,
            environment,
            command,
            process_command,
            start_directory: None,
            keep_alive_on_exit: None,
        }
    }
}
