//! Detached response contracts.

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{ControlModeResponse, HandshakeResponse, LayoutName, RmuxError, SdkWaitId};

#[path = "response/session.rs"]
mod session;
pub use session::{
    CreateSessionLeaseResponse, HasSessionResponse, KillSessionResponse, ListSessionsResponse,
    NewSessionResponse, ReleaseSessionLeaseResponse, RenameSessionResponse,
    RenewSessionLeaseResponse,
};

#[path = "response/server.rs"]
mod server;
pub use server::{
    DaemonStatusResponse, KillServerResponse, LockClientResponse, LockServerResponse,
    LockSessionResponse, ServerAccessResponse, ShutdownIfIdleResponse,
};

#[path = "response/target.rs"]
mod target;
pub use target::ResolveTargetResponse;

#[path = "response/window.rs"]
mod window;
pub use window::{
    KillWindowResponse, LastWindowResponse, LinkWindowResponse, ListWindowsResponse,
    MoveWindowResponse, NewWindowResponse, NextWindowResponse, PreviousWindowResponse,
    RenameWindowResponse, ResizeWindowResponse, RespawnWindowResponse, RotateWindowResponse,
    SelectWindowResponse, SwapWindowResponse, UnlinkWindowResponse, WindowListEntry,
};

#[path = "response/pane.rs"]
mod pane;
pub use pane::{
    BreakPaneResponse, DisplayPanesResponse, JoinPaneResponse, KillPaneResponse, LastPaneResponse,
    ListPanesResponse, MovePaneResponse, PaneBroadcastInputFailure, PaneBroadcastInputResponse,
    PaneBroadcastInputSuccess, PaneOutputCursor, PaneOutputCursorResponse, PaneOutputEvent,
    PaneOutputLagNotice, PaneOutputLagResponse, PaneRecentOutput, PaneSnapshotCell,
    PaneSnapshotCursor, PaneSnapshotResponse, PipePaneResponse, ResizePaneResponse,
    RespawnPaneResponse, SelectPaneResponse, SendKeysResponse, SplitWindowResponse,
    SubscribePaneOutputResponse, SwapPaneResponse, UnsubscribePaneOutputResponse,
};

#[path = "response/client.rs"]
mod client;
pub use client::{
    AttachSessionResponse, DetachClientResponse, ListClientsResponse, RefreshClientResponse,
    SuspendClientResponse, SwitchClientResponse,
};

#[path = "response/keys.rs"]
mod keys;
pub use keys::{
    BindKeyResponse, ClockModeResponse, CopyModeResponse, ListKeysResponse, SendPrefixResponse,
    UnbindKeyResponse,
};

#[path = "response/options.rs"]
mod options;
pub use options::{
    SetEnvironmentResponse, SetHookResponse, SetOptionByNameResponse, SetOptionResponse,
    ShowEnvironmentResponse, ShowHooksResponse, ShowOptionsResponse,
};

#[path = "response/web.rs"]
mod web;
pub use web::{
    WebShareConfigResponse, WebShareCreatedResponse, WebShareListResponse, WebShareListener,
    WebShareLookupResponse, WebShareResponse, WebShareStoppedAllResponse, WebShareStoppedResponse,
    WebShareSummary,
};

/// All detached responses supported by the wire protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// Success payload for `new-session`.
    NewSession(NewSessionResponse),
    /// Success payload for `has-session`.
    HasSession(HasSessionResponse),
    /// Success payload for `kill-session`.
    KillSession(KillSessionResponse),
    /// Success payload for `new-window`.
    NewWindow(NewWindowResponse),
    /// Success payload for `kill-window`.
    KillWindow(KillWindowResponse),
    /// Success payload for `select-window`.
    SelectWindow(SelectWindowResponse),
    /// Success payload for `rename-window`.
    RenameWindow(RenameWindowResponse),
    /// Success payload for `next-window`.
    NextWindow(NextWindowResponse),
    /// Success payload for `previous-window`.
    PreviousWindow(PreviousWindowResponse),
    /// Success payload for `last-window`.
    LastWindow(LastWindowResponse),
    /// Success payload for `list-windows`.
    ListWindows(ListWindowsResponse),
    /// Success payload for `move-window`.
    MoveWindow(MoveWindowResponse),
    /// Success payload for `swap-window`.
    SwapWindow(SwapWindowResponse),
    /// Success payload for `rotate-window`.
    RotateWindow(RotateWindowResponse),
    /// Success payload for `split-window`.
    SplitWindow(SplitWindowResponse),
    /// Success payload for `swap-pane`.
    SwapPane(SwapPaneResponse),
    /// Success payload for `last-pane`.
    LastPane(LastPaneResponse),
    /// Success payload for `join-pane`.
    JoinPane(JoinPaneResponse),
    /// Success payload for `break-pane`.
    BreakPane(BreakPaneResponse),
    /// Success payload for `kill-pane`.
    KillPane(KillPaneResponse),
    /// Success payload for `select-layout`.
    SelectLayout(SelectLayoutResponse),
    /// Success payload for `resize-pane`.
    ResizePane(ResizePaneResponse),
    /// Success payload for `display-panes`.
    DisplayPanes(DisplayPanesResponse),
    /// Success payload for `select-pane`.
    SelectPane(SelectPaneResponse),
    /// Success payload for `send-keys`.
    SendKeys(SendKeysResponse),
    /// Success payload for `attach-session`.
    AttachSession(AttachSessionResponse),
    /// Success payload for `switch-client`.
    SwitchClient(SwitchClientResponse),
    /// Success payload for `detach-client`.
    DetachClient(DetachClientResponse),
    /// Success payload for `set-option`.
    SetOption(SetOptionResponse),
    /// Success payload for `set-environment`.
    SetEnvironment(SetEnvironmentResponse),
    /// Success payload for `set-hook`.
    SetHook(SetHookResponse),
    /// Error payload surfaced as exit code `1` and stderr by clients.
    Error(ErrorResponse),
    /// Success payload for `next-layout`.
    NextLayout(NextLayoutResponse),
    /// Success payload for `previous-layout`.
    PreviousLayout(PreviousLayoutResponse),
    /// Success payload for `show-options`.
    ShowOptions(ShowOptionsResponse),
    /// Success payload for `show-environment`.
    ShowEnvironment(ShowEnvironmentResponse),
    /// Success payload for `set-buffer`.
    SetBuffer(SetBufferResponse),
    /// Success payload for `show-buffer`.
    ShowBuffer(ShowBufferResponse),
    /// Success payload for `paste-buffer`.
    PasteBuffer(PasteBufferResponse),
    /// Success payload for `list-buffers`.
    ListBuffers(ListBuffersResponse),
    /// Success payload for `delete-buffer`.
    DeleteBuffer(DeleteBufferResponse),
    /// Success payload for `load-buffer`.
    LoadBuffer(LoadBufferResponse),
    /// Success payload for `save-buffer`.
    SaveBuffer(SaveBufferResponse),
    /// Success payload for `capture-pane`.
    CapturePane(CapturePaneResponse),
    /// Success payload for `display-message`.
    DisplayMessage(DisplayMessageResponse),
    /// Success payload for `run-shell`.
    RunShell(RunShellResponse),
    /// Success payload for `if-shell`.
    IfShell(IfShellResponse),
    /// Success payload for `wait-for`.
    WaitFor(WaitForResponse),
    /// Success payload for `rename-session`.
    RenameSession(RenameSessionResponse),
    /// Success payload for `list-sessions`.
    ListSessions(ListSessionsResponse),
    /// Success payload for `list-panes`.
    ListPanes(ListPanesResponse),
    /// Success payload for `source-file`.
    SourceFile(SourceFileResponse),
    /// Success payload for string-based `set-option`.
    SetOptionByName(SetOptionByNameResponse),
    /// Success payload for `show-hooks`.
    ShowHooks(ShowHooksResponse),
    /// Success payload for `bind-key`.
    BindKey(BindKeyResponse),
    /// Success payload for `unbind-key`.
    UnbindKey(UnbindKeyResponse),
    /// Success payload for `list-keys`.
    ListKeys(ListKeysResponse),
    /// Success payload for `send-prefix`.
    SendPrefix(SendPrefixResponse),
    /// Success payload for `clear-history`.
    ClearHistory(ClearHistoryResponse),
    /// Success payload for `copy-mode`.
    CopyMode(CopyModeResponse),
    /// Success payload for detached control-mode upgrade.
    ControlMode(ControlModeResponse),
    /// Success payload for `clock-mode`.
    ClockMode(ClockModeResponse),
    /// Success payload for `show-messages`.
    ShowMessages(ShowMessagesResponse),
    /// Success payload for `kill-server`.
    KillServer(KillServerResponse),
    /// Success payload for `lock-server`.
    LockServer(LockServerResponse),
    /// Success payload for `lock-session`.
    LockSession(LockSessionResponse),
    /// Success payload for `lock-client`.
    LockClient(LockClientResponse),
    /// Success payload for `server-access`.
    ServerAccess(ServerAccessResponse),
    /// Success payload for `refresh-client`.
    RefreshClient(RefreshClientResponse),
    /// Success payload for `list-clients`.
    ListClients(ListClientsResponse),
    /// Success payload for `suspend-client`.
    SuspendClient(SuspendClientResponse),
    /// Success payload for `resize-window`.
    ResizeWindow(ResizeWindowResponse),
    /// Success payload for `respawn-window`.
    RespawnWindow(RespawnWindowResponse),
    /// Success payload for `move-pane`.
    MovePane(MovePaneResponse),
    /// Success payload for `pipe-pane`.
    PipePane(PipePaneResponse),
    /// Success payload for `respawn-pane`.
    RespawnPane(RespawnPaneResponse),
    /// Success payload for `link-window`.
    LinkWindow(LinkWindowResponse),
    /// Success payload for `unlink-window`.
    UnlinkWindow(UnlinkWindowResponse),
    /// Success payload for internal detached target resolution.
    ResolveTarget(ResolveTargetResponse),
    /// Success payload for SDK/daemon version and capability negotiation.
    Handshake(HandshakeResponse),
    /// Success payload for the daemon-backed pane snapshot endpoint.
    PaneSnapshot(PaneSnapshotResponse),
    /// Success payload for the daemon-backed pane output subscription endpoint.
    SubscribePaneOutput(SubscribePaneOutputResponse),
    /// Success payload for the daemon-backed pane output unsubscription endpoint.
    UnsubscribePaneOutput(UnsubscribePaneOutputResponse),
    /// Success payload for daemon-backed pane output cursor polling.
    PaneOutputCursor(PaneOutputCursorResponse),
    /// Lag notice for daemon-backed pane output cursor polling.
    PaneOutputLag(Box<PaneOutputLagResponse>),
    /// Success payload for a daemon-backed SDK byte wait.
    SdkWaitForOutput(SdkWaitForOutputResponse),
    /// Success payload for daemon-backed SDK wait cancellation.
    CancelSdkWait(CancelSdkWaitResponse),
    /// Success payload for daemon-side SDK pane-input broadcast.
    PaneBroadcastInput(PaneBroadcastInputResponse),
    /// Success payload for creating an app-owned session lease.
    CreateSessionLease(CreateSessionLeaseResponse),
    /// Success payload for renewing an app-owned session lease.
    RenewSessionLease(RenewSessionLeaseResponse),
    /// Success payload for releasing an app-owned session lease.
    ReleaseSessionLease(ReleaseSessionLeaseResponse),
    /// Success payload for internal daemon version and activity status.
    DaemonStatus(DaemonStatusResponse),
    /// Success payload for internal idle-only daemon shutdown.
    ShutdownIfIdle(ShutdownIfIdleResponse),
    /// Success payload for browser-visible pane sharing.
    WebShare(Box<WebShareResponse>),
}

impl Response {
    /// Returns the stable routing name for the response variant.
    ///
    /// `Error` is not tied to one command on the current wire and therefore
    /// reports the generic `error` tag.
    #[must_use]
    pub const fn command_name(&self) -> &'static str {
        match self {
            Self::NewSession(_) => "new-session",
            Self::HasSession(_) => "has-session",
            Self::KillSession(_) => "kill-session",
            Self::NewWindow(_) => "new-window",
            Self::KillWindow(_) => "kill-window",
            Self::SelectWindow(_) => "select-window",
            Self::RenameWindow(_) => "rename-window",
            Self::NextWindow(_) => "next-window",
            Self::PreviousWindow(_) => "previous-window",
            Self::LastWindow(_) => "last-window",
            Self::ListWindows(_) => "list-windows",
            Self::MoveWindow(_) => "move-window",
            Self::SwapWindow(_) => "swap-window",
            Self::RotateWindow(_) => "rotate-window",
            Self::SplitWindow(_) => "split-window",
            Self::SwapPane(_) => "swap-pane",
            Self::LastPane(_) => "last-pane",
            Self::JoinPane(_) => "join-pane",
            Self::BreakPane(_) => "break-pane",
            Self::KillPane(_) => "kill-pane",
            Self::SelectLayout(_) => "select-layout",
            Self::ResizePane(_) => "resize-pane",
            Self::DisplayPanes(_) => "display-panes",
            Self::SelectPane(_) => "select-pane",
            Self::SendKeys(_) => "send-keys",
            Self::AttachSession(_) => "attach-session",
            Self::SwitchClient(_) => "switch-client",
            Self::DetachClient(_) => "detach-client",
            Self::SetOption(_) | Self::SetOptionByName(_) => "set-option",
            Self::SetEnvironment(_) => "set-environment",
            Self::SetHook(_) => "set-hook",
            Self::Error(_) => "error",
            Self::NextLayout(_) => "next-layout",
            Self::PreviousLayout(_) => "previous-layout",
            Self::ShowOptions(_) => "show-options",
            Self::ShowEnvironment(_) => "show-environment",
            Self::SetBuffer(_) => "set-buffer",
            Self::ShowBuffer(_) => "show-buffer",
            Self::PasteBuffer(_) => "paste-buffer",
            Self::ListBuffers(_) => "list-buffers",
            Self::DeleteBuffer(_) => "delete-buffer",
            Self::LoadBuffer(_) => "load-buffer",
            Self::SaveBuffer(_) => "save-buffer",
            Self::CapturePane(_) => "capture-pane",
            Self::DisplayMessage(_) => "display-message",
            Self::RunShell(_) => "run-shell",
            Self::IfShell(_) => "if-shell",
            Self::WaitFor(_) => "wait-for",
            Self::RenameSession(_) => "rename-session",
            Self::ListSessions(_) => "list-sessions",
            Self::ListPanes(_) => "list-panes",
            Self::SourceFile(_) => "source-file",
            Self::ShowHooks(_) => "show-hooks",
            Self::BindKey(_) => "bind-key",
            Self::UnbindKey(_) => "unbind-key",
            Self::ListKeys(_) => "list-keys",
            Self::SendPrefix(_) => "send-prefix",
            Self::ClearHistory(_) => "clear-history",
            Self::CopyMode(_) => "copy-mode",
            Self::ControlMode(_) => "control-mode",
            Self::ClockMode(_) => "clock-mode",
            Self::ShowMessages(_) => "show-messages",
            Self::KillServer(_) => "kill-server",
            Self::LockServer(_) => "lock-server",
            Self::LockSession(_) => "lock-session",
            Self::LockClient(_) => "lock-client",
            Self::ServerAccess(_) => "server-access",
            Self::RefreshClient(_) => "refresh-client",
            Self::ListClients(_) => "list-clients",
            Self::SuspendClient(_) => "suspend-client",
            Self::ResizeWindow(_) => "resize-window",
            Self::RespawnWindow(_) => "respawn-window",
            Self::MovePane(_) => "move-pane",
            Self::PipePane(_) => "pipe-pane",
            Self::RespawnPane(_) => "respawn-pane",
            Self::PaneSnapshot(_) => "pane-snapshot",
            Self::SubscribePaneOutput(_) => "subscribe-pane-output",
            Self::UnsubscribePaneOutput(_) => "unsubscribe-pane-output",
            Self::PaneOutputCursor(_) => "pane-output-cursor",
            Self::PaneOutputLag(_) => "pane-output-lag",
            Self::SdkWaitForOutput(_) => "sdk-wait-output",
            Self::CancelSdkWait(_) => "cancel-sdk-wait",
            Self::PaneBroadcastInput(_) => "send-keys",
            Self::CreateSessionLease(_) => "create-session-lease",
            Self::RenewSessionLease(_) => "renew-session-lease",
            Self::ReleaseSessionLease(_) => "release-session-lease",
            Self::LinkWindow(_) => "link-window",
            Self::UnlinkWindow(_) => "unlink-window",
            Self::ResolveTarget(_) => "resolve-target",
            Self::Handshake(_) => "handshake",
            Self::DaemonStatus(_) => "daemon-status",
            Self::ShutdownIfIdle(_) => "shutdown-if-idle",
            Self::WebShare(_) => "web-share",
        }
    }

    /// Returns `true` when this response carries an error payload.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Returns the shared stdout contract when this response carries printable output.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        match self {
            Self::NewSession(response) => response.command_output(),
            Self::ListWindows(response) => Some(response.command_output()),
            Self::ShowOptions(response) => Some(response.command_output()),
            Self::ShowEnvironment(response) => Some(response.command_output()),
            Self::ShowHooks(response) => Some(response.command_output()),
            Self::ShowBuffer(response) => Some(response.command_output()),
            Self::ListBuffers(response) => Some(response.command_output()),
            Self::CapturePane(response) => response.command_output(),
            Self::DisplayMessage(response) => response.command_output(),
            Self::ResolveTarget(_) => None,
            Self::RunShell(response) => response.command_output(),
            Self::IfShell(response) => response.command_output(),
            Self::ListSessions(response) => Some(response.command_output()),
            Self::ListPanes(response) => Some(response.command_output()),
            Self::SourceFile(response) => response.command_output(),
            Self::SetOptionByName(_) => None,
            Self::ListKeys(response) => Some(response.command_output()),
            Self::ControlMode(_) => None,
            Self::ClockMode(_) => None,
            Self::ShowMessages(response) => Some(response.command_output()),
            Self::ServerAccess(response) => Some(response.command_output()),
            Self::ListClients(response) => Some(response.command_output()),
            Self::BreakPane(response) => response.command_output(),
            Self::Handshake(_) => None,
            Self::WebShare(response) => response.command_output(),
            Self::SubscribePaneOutput(_)
            | Self::UnsubscribePaneOutput(_)
            | Self::PaneOutputCursor(_)
            | Self::PaneOutputLag(_)
            | Self::SdkWaitForOutput(_)
            | Self::CancelSdkWait(_)
            | Self::PaneBroadcastInput(_)
            | Self::CreateSessionLease(_)
            | Self::RenewSessionLease(_)
            | Self::ReleaseSessionLease(_)
            | Self::DaemonStatus(_)
            | Self::ShutdownIfIdle(_) => None,
            _ => None,
        }
    }
}

/// Reusable stdout payload for commands that produce printable output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    /// The exact bytes the client should write to stdout.
    pub stdout: Vec<u8>,
}

impl CommandOutput {
    /// Creates a stdout payload from raw bytes.
    #[must_use]
    pub fn from_stdout(stdout: impl Into<Vec<u8>>) -> Self {
        Self {
            stdout: stdout.into(),
        }
    }

    /// Returns the raw stdout bytes.
    #[must_use]
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }
}

/// Response payload for `select-layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectLayoutResponse {
    /// The applied layout.
    pub layout: LayoutName,
}

/// Response payload for `next-layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextLayoutResponse {
    /// The applied layout.
    pub layout: LayoutName,
}

/// Response payload for `previous-layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousLayoutResponse {
    /// The applied layout.
    pub layout: LayoutName,
}

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

/// Response payload for `capture-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturePaneResponse {
    /// The created/replaced buffer name for non-printing captures.
    pub buffer_name: Option<String>,
    /// Captured stdout for `capture-pane -p`.
    pub output: Option<CommandOutput>,
}

impl CapturePaneResponse {
    /// Builds a non-printing capture response.
    #[must_use]
    pub fn from_buffer(buffer_name: String) -> Self {
        Self {
            buffer_name: Some(buffer_name),
            output: None,
        }
    }

    /// Builds a printing capture response.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            buffer_name: None,
            output: Some(output),
        }
    }

    /// Returns the reusable stdout payload when this was a printing capture.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }
}

/// Response payload for `clear-history`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearHistoryResponse;

/// Response payload for `show-messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowMessagesResponse {
    /// Expanded stdout for `show-messages`.
    pub output: CommandOutput,
}

impl ShowMessagesResponse {
    /// Builds a response from reusable command output bytes.
    #[must_use]
    pub const fn from_output(output: CommandOutput) -> Self {
        Self { output }
    }

    /// Returns the stdout payload for `show-messages`.
    #[must_use]
    pub const fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}

/// Response payload for `display-message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayMessageResponse {
    /// Expanded stdout for `display-message -p`.
    pub output: Option<CommandOutput>,
}

impl DisplayMessageResponse {
    /// Builds a non-printing display response.
    #[must_use]
    pub const fn no_output() -> Self {
        Self { output: None }
    }

    /// Builds a printing display response.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
        }
    }

    /// Returns the reusable stdout payload when this was a printing display.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }
}

/// Response payload for `run-shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunShellResponse {
    /// Captured stdout for foreground `run-shell`.
    pub output: Option<CommandOutput>,
    /// Exit status returned by a foreground shell command.
    pub exit_status: Option<i32>,
}

impl RunShellResponse {
    /// Builds a background `run-shell` response.
    #[must_use]
    pub const fn background() -> Self {
        Self {
            output: None,
            exit_status: None,
        }
    }

    /// Builds a response with no command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self::background()
    }

    /// Builds a foreground `run-shell` response.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
            exit_status: Some(0),
        }
    }

    /// Builds a foreground shell response with an exact exit status.
    #[must_use]
    pub const fn from_exit_status(exit_status: i32) -> Self {
        Self {
            output: None,
            exit_status: Some(exit_status),
        }
    }

    /// Builds a foreground shell response with output and an exact exit status.
    #[must_use]
    pub fn from_output_and_exit_status(output: CommandOutput, exit_status: i32) -> Self {
        Self {
            output: Some(output),
            exit_status: Some(exit_status),
        }
    }

    /// Returns the reusable stdout payload for foreground `run-shell`.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }

    /// Returns the shell exit status when the response came from a foreground shell.
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
}

impl<'de> Deserialize<'de> for RunShellResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "RunShellResponse",
            &["output", "exit_status"],
            RunShellResponseVisitor,
        )
    }
}

struct RunShellResponseVisitor;

impl<'de> Visitor<'de> for RunShellResponseVisitor {
    type Value = RunShellResponse;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a run-shell response")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let output = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let exit_status = compat_next_response_element(&mut seq)?;
        Ok(RunShellResponse {
            output,
            exit_status,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut output = None;
        let mut exit_status = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "output" => output = Some(map.next_value()?),
                "exit_status" => exit_status = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(RunShellResponse {
            output: output.ok_or_else(|| de::Error::missing_field("output"))?,
            exit_status: exit_status.unwrap_or_default(),
        })
    }
}

fn compat_next_response_element<'de, A, T>(seq: &mut A) -> Result<T, A::Error>
where
    A: SeqAccess<'de>,
    T: Deserialize<'de> + Default,
{
    match seq.next_element::<T>() {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Ok(T::default()),
        Err(error) if is_truncated_compat_sequence(&error) => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn is_truncated_compat_sequence(error: &impl std::fmt::Display) -> bool {
    let message = error.to_string();
    message.contains("UnexpectedEof") || message.contains("unexpected end of file")
}

/// Response payload for `if-shell`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IfShellResponse {
    /// Captured stdout from the selected nested command when it produced output.
    pub output: Option<CommandOutput>,
}

impl IfShellResponse {
    /// Builds a response with no nested command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self { output: None }
    }

    /// Builds a response with nested command output.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
        }
    }

    /// Returns the reusable stdout payload when the selected nested command printed output.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }
}

/// Response payload for `source-file`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourceFileResponse {
    /// Verbose parsed-command output, when requested.
    pub output: Option<CommandOutput>,
    /// Exit status surfaced by a nested foreground command.
    pub exit_status: Option<i32>,
    /// File-loading diagnostics that tmux writes to stderr.
    pub stderr: Vec<u8>,
}

impl SourceFileResponse {
    /// Builds a response without command output.
    #[must_use]
    pub const fn no_output() -> Self {
        Self {
            output: None,
            stderr: Vec::new(),
            exit_status: None,
        }
    }

    /// Builds a response with command output.
    #[must_use]
    pub fn from_output(output: CommandOutput) -> Self {
        Self {
            output: Some(output),
            stderr: Vec::new(),
            exit_status: None,
        }
    }

    /// Returns the reusable stdout payload when `source-file -v` printed output.
    #[must_use]
    pub fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }

    /// Returns bytes to write to stderr for file-loading diagnostics.
    #[must_use]
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    /// Adds stderr bytes to the response.
    #[must_use]
    pub fn with_stderr(mut self, stderr: Vec<u8>) -> Self {
        self.stderr = stderr;
        self
    }

    /// Adds a command exit status to the response.
    #[must_use]
    pub const fn with_exit_status(mut self, exit_status: Option<i32>) -> Self {
        self.exit_status = exit_status;
        self
    }

    /// Returns the surfaced nested command exit status, if any.
    #[must_use]
    pub const fn exit_status(&self) -> Option<i32> {
        self.exit_status
    }
}

impl<'de> Deserialize<'de> for SourceFileResponse {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SourceFileResponse",
            &["output", "exit_status", "stderr"],
            SourceFileResponseVisitor,
        )
    }
}

struct SourceFileResponseVisitor;

impl<'de> Visitor<'de> for SourceFileResponseVisitor {
    type Value = SourceFileResponse;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a source-file response")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let output = seq
            .next_element()?
            .ok_or_else(|| de::Error::invalid_length(0, &self))?;
        let exit_status = compat_next_response_element(&mut seq)?;
        let stderr = compat_next_response_element(&mut seq)?;
        Ok(SourceFileResponse {
            output,
            exit_status,
            stderr,
        })
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut output = None;
        let mut exit_status = None;
        let mut stderr = None;

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "output" => output = Some(map.next_value()?),
                "exit_status" => exit_status = Some(map.next_value()?),
                "stderr" => stderr = Some(map.next_value()?),
                _ => {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
        }

        Ok(SourceFileResponse {
            output: output.ok_or_else(|| de::Error::missing_field("output"))?,
            exit_status: exit_status.unwrap_or_default(),
            stderr: stderr.unwrap_or_default(),
        })
    }
}

/// Response payload for `wait-for`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForResponse;

/// Terminal state for a daemon-backed SDK byte wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SdkWaitOutcome {
    /// The requested byte sequence was observed.
    Matched,
    /// The wait was cancelled by a best-effort SDK cancel request or
    /// connection cleanup.
    Cancelled,
}

/// Response payload for a daemon-backed SDK byte wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkWaitForOutputResponse {
    /// Wait ID that completed.
    pub wait_id: SdkWaitId,
    /// Terminal wait outcome.
    pub outcome: SdkWaitOutcome,
}

/// Response payload for best-effort daemon-backed SDK wait cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelSdkWaitResponse {
    /// Wait ID named by the cancellation request.
    pub wait_id: SdkWaitId,
    /// Whether a live wait was removed by this request.
    pub removed: bool,
}

impl CancelSdkWaitResponse {
    /// Returns the compatibility-preserving ACK used by armed SDK waits.
    ///
    /// `SdkWaitOutcome` is a published exhaustive enum whose terminal outcomes
    /// must stay source-compatible across patch releases. The daemon therefore
    /// acknowledges an armed wait with this existing response shape instead of
    /// adding a third `SdkWaitOutcome` variant.
    #[must_use]
    pub const fn armed_ack(wait_id: SdkWaitId) -> Self {
        Self {
            wait_id,
            removed: false,
        }
    }

    /// Returns whether this response is the compatibility ACK for `wait_id`.
    #[must_use]
    pub const fn is_armed_ack_for(&self, wait_id: SdkWaitId) -> bool {
        self.wait_id.as_u64() == wait_id.as_u64() && !self.removed
    }
}

/// Error response payload for detached RPC failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// The shared wire-safe error value.
    pub error: RmuxError,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OptionScopeSelector, PaneOutputSubscriptionId, SetOptionMode};
    use serde::Serialize;
    use std::mem::size_of;

    #[derive(Serialize)]
    struct OldRunShellResponse {
        output: Option<CommandOutput>,
    }

    #[derive(Serialize)]
    struct OldSourceFileResponse {
        output: Option<CommandOutput>,
        exit_status: Option<i32>,
    }

    #[test]
    fn run_shell_response_deserializes_old_payloads_with_no_exit_status() {
        let bytes = bincode::serialize(&OldRunShellResponse { output: None })
            .expect("old run-shell response serializes");

        let decoded: RunShellResponse =
            bincode::deserialize(&bytes).expect("new run-shell response decodes old payload");

        assert_eq!(decoded.command_output(), None);
        assert_eq!(decoded.exit_status(), None);
    }

    #[test]
    fn source_file_response_deserializes_old_payloads_with_empty_stderr() {
        let bytes = bincode::serialize(&OldSourceFileResponse {
            output: Some(CommandOutput::from_stdout(b"parsed\n".to_vec())),
            exit_status: Some(1),
        })
        .expect("old source-file response serializes");

        let decoded: SourceFileResponse =
            bincode::deserialize(&bytes).expect("new source-file response decodes old payload");

        assert_eq!(
            decoded.command_output(),
            Some(&CommandOutput::from_stdout(b"parsed\n".to_vec()))
        );
        assert_eq!(decoded.exit_status(), Some(1));
        assert!(decoded.stderr().is_empty());
    }

    #[test]
    fn response_command_names_cover_base_aliases_and_error_tag() {
        assert_eq!(
            Response::HasSession(HasSessionResponse { exists: true }).command_name(),
            "has-session"
        );
        assert_eq!(
            Response::SetOptionByName(SetOptionByNameResponse {
                scope: OptionScopeSelector::ServerGlobal,
                name: "status".to_owned(),
                mode: SetOptionMode::Replace,
            })
            .command_name(),
            "set-option"
        );
        assert_eq!(
            Response::KillServer(KillServerResponse).command_name(),
            "kill-server"
        );
        assert_eq!(
            Response::Error(ErrorResponse {
                error: RmuxError::Server("failed".to_owned()),
            })
            .command_name(),
            "error"
        );
        assert_eq!(
            Response::Handshake(HandshakeResponse::current()).command_name(),
            "handshake"
        );
        assert_eq!(
            Response::SdkWaitForOutput(SdkWaitForOutputResponse {
                wait_id: SdkWaitId::new(1),
                outcome: SdkWaitOutcome::Matched,
            })
            .command_name(),
            "sdk-wait-output"
        );
        assert_eq!(
            Response::CancelSdkWait(CancelSdkWaitResponse {
                wait_id: SdkWaitId::new(1),
                removed: false,
            })
            .command_name(),
            "cancel-sdk-wait"
        );
        assert_eq!(
            Response::WaitFor(WaitForResponse).command_name(),
            "wait-for"
        );
    }

    #[test]
    fn sdk_wait_outcome_patch_surface_stays_terminal_only() {
        fn is_terminal(outcome: SdkWaitOutcome) -> bool {
            match outcome {
                SdkWaitOutcome::Matched | SdkWaitOutcome::Cancelled => true,
            }
        }

        assert!(is_terminal(SdkWaitOutcome::Matched));
        assert!(is_terminal(SdkWaitOutcome::Cancelled));
    }

    #[test]
    fn pr6g_response_boxing_keeps_response_size_bounded() {
        assert_eq!(size_of::<Box<WebShareResponse>>(), 8);
        assert_eq!(size_of::<Box<PaneOutputLagResponse>>(), 8);
        assert_eq!(
            size_of::<Response>(),
            72,
            "Response should stay compact after boxing the two largest payload variants"
        );
        assert!(
            size_of::<WebShareResponse>() > size_of::<Response>(),
            "WebShareResponse must remain boxed while it is larger than Response"
        );
        assert!(
            size_of::<PaneOutputLagResponse>() > size_of::<Response>(),
            "PaneOutputLagResponse must remain boxed while it is larger than Response"
        );
    }

    #[test]
    fn pr6g_boxed_response_payloads_are_bincode_transparent() {
        fn assert_transparent<T>(response: Response, payload: &T)
        where
            T: Serialize,
        {
            let mut expected = crate::frame_kind_for_response(&response)
                .bincode_tag()
                .to_le_bytes()
                .to_vec();
            expected.extend(bincode::serialize(payload).expect("payload encodes"));
            assert_eq!(
                bincode::serialize(&response).expect("response encodes"),
                expected
            );
        }

        let lag = PaneOutputLagResponse {
            subscription_id: PaneOutputSubscriptionId::new(7),
            cursor: PaneOutputCursor {
                next_sequence: 10,
                missed_events: 4,
            },
            lag: PaneOutputLagNotice {
                expected_sequence: 6,
                resume_sequence: 10,
                missed_events: 4,
                newest_sequence: 12,
                recent: PaneRecentOutput {
                    bytes: b"recent".to_vec(),
                    oldest_sequence: Some(10),
                    newest_sequence: Some(12),
                },
            },
        };
        assert_transparent(Response::PaneOutputLag(Box::new(lag.clone())), &lag);

        let web_share = WebShareResponse::Config(WebShareConfigResponse {
            listener: WebShareListener {
                host: "127.0.0.1".to_owned(),
                port: 9777,
                frontend_origin: "https://share.rmux.io".to_owned(),
            },
            output: CommandOutput::from_stdout("127.0.0.1:9777 https://share.rmux.io\n"),
        });
        assert_transparent(Response::WebShare(Box::new(web_share.clone())), &web_share);
    }
}
