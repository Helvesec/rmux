//! Detached request contracts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    ControlModeRequest, HandshakeRequest, PaneTarget, PaneTargetRef, SdkWaitId, SdkWaitOwnerId,
    SessionName, Target, WindowTarget,
};

#[path = "request/show.rs"]
mod show;
pub use show::ShowHooksRequest;
pub use show::{ShowEnvironmentRequest, ShowOptionsRequest};

#[path = "request/layout.rs"]
mod layout;
pub use layout::{
    NextLayoutRequest, PreviousLayoutRequest, SelectCustomLayoutRequest, SelectLayoutRequest,
    SelectLayoutTarget, SelectOldLayoutRequest, SpreadLayoutRequest,
};

#[path = "request/pane.rs"]
mod pane;
pub use pane::{
    BreakPaneRequest, DisplayPanesRequest, JoinPaneRequest, KillPaneRequest, LastPaneRequest,
    MovePaneRequest, PaneBroadcastInputRequest, PaneInputRequest, PaneKillRequest,
    PaneOutputCursorRequest, PaneOutputSubscriptionStart, PaneResizeRequest, PaneRespawnRequest,
    PaneSelectRequest, PaneSnapshotRefRequest, PaneSnapshotRequest, PaneSplitSize, PipePaneRequest,
    ResizePaneRequest, RespawnPaneRequest, SelectPaneAdjacentRequest, SelectPaneDirection,
    SelectPaneMarkRequest, SelectPaneRequest, SendKeysExtRequest, SendKeysRequest,
    SplitWindowExtRequest, SplitWindowRequest, SplitWindowTarget, SubscribePaneOutputRefRequest,
    SubscribePaneOutputRequest, SwapPaneDirection, SwapPaneRequest, UnsubscribePaneOutputRequest,
};

#[path = "request/window.rs"]
mod window;
pub use window::{
    KillWindowRequest, LastWindowRequest, LinkWindowRequest, ListWindowsRequest, MoveWindowRequest,
    MoveWindowTarget, NewWindowRequest, NextWindowRequest, PreviousWindowRequest,
    RenameWindowRequest, ResizeWindowAdjustment, ResizeWindowRequest, RespawnWindowRequest,
    RotateWindowDirection, RotateWindowRequest, SelectWindowRequest, SwapWindowRequest,
};

#[path = "request/target.rs"]
mod target;
pub use target::{ResolveTargetRequest, ResolveTargetType};

#[path = "request/session.rs"]
mod session;
pub use session::{
    CreateSessionLeaseRequest, HasSessionRequest, KillSessionRequest, ListSessionsRequest,
    NewSessionExtRequest, NewSessionRequest, ReleaseSessionLeaseRequest, RenameSessionRequest,
    RenewSessionLeaseRequest,
};

#[path = "request/server.rs"]
mod server;
pub use server::{
    KillServerRequest, LockClientRequest, LockServerRequest, LockSessionRequest,
    ServerAccessRequest,
};

#[path = "request/client.rs"]
mod client;
pub use client::{
    AttachSessionExt2Request, AttachSessionExtRequest, AttachSessionRequest,
    DetachClientExtRequest, DetachClientRequest, ListClientsRequest, RefreshClientRequest,
    SuspendClientRequest, SwitchClientExt2Request, SwitchClientExt3Request, SwitchClientExtRequest,
    SwitchClientRequest,
};

#[path = "request/keys.rs"]
mod keys;
pub use keys::{
    BindKeyRequest, ClockModeRequest, CopyModeRequest, ListKeysRequest, SendPrefixRequest,
    UnbindKeyRequest,
};

#[path = "request/options.rs"]
mod options;
pub use options::{
    SetEnvironmentMode, SetEnvironmentRequest, SetHookMutationRequest, SetHookRequest,
    SetOptionByNameRequest, SetOptionRequest,
};

#[path = "request/buffer.rs"]
mod buffer;
pub use buffer::{
    CapturePaneRequest, ClearHistoryRequest, DeleteBufferRequest, ListBuffersRequest,
    LoadBufferRequest, PasteBufferRequest, SaveBufferRequest, SetBufferRequest, ShowBufferRequest,
};

/// All detached public command and internal RPC requests supported by the wire
/// protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// `new-session`
    NewSession(NewSessionRequest),
    /// `has-session`
    HasSession(HasSessionRequest),
    /// `kill-session`
    KillSession(KillSessionRequest),
    /// `new-window`
    NewWindow(NewWindowRequest),
    /// `kill-window`
    KillWindow(KillWindowRequest),
    /// `select-window`
    SelectWindow(SelectWindowRequest),
    /// `rename-window`
    RenameWindow(RenameWindowRequest),
    /// `next-window`
    NextWindow(NextWindowRequest),
    /// `previous-window`
    PreviousWindow(PreviousWindowRequest),
    /// `last-window`
    LastWindow(LastWindowRequest),
    /// `list-windows`
    ListWindows(ListWindowsRequest),
    /// `move-window`
    MoveWindow(MoveWindowRequest),
    /// `swap-window`
    SwapWindow(SwapWindowRequest),
    /// `rotate-window`
    RotateWindow(RotateWindowRequest),
    /// `split-window`
    SplitWindow(SplitWindowRequest),
    /// `swap-pane`
    SwapPane(SwapPaneRequest),
    /// `last-pane`
    LastPane(LastPaneRequest),
    /// `join-pane`
    JoinPane(JoinPaneRequest),
    /// `break-pane`
    BreakPane(BreakPaneRequest),
    /// `kill-pane`
    KillPane(KillPaneRequest),
    /// `select-layout`
    SelectLayout(SelectLayoutRequest),
    /// `resize-pane`
    ResizePane(ResizePaneRequest),
    /// `display-panes`
    DisplayPanes(DisplayPanesRequest),
    /// `select-pane`
    SelectPane(SelectPaneRequest),
    /// `select-pane -U/-D/-L/-R`
    SelectPaneAdjacent(SelectPaneAdjacentRequest),
    /// `send-keys`
    SendKeys(SendKeysRequest),
    /// `attach-session`
    AttachSession(AttachSessionRequest),
    /// `switch-client`
    SwitchClient(SwitchClientRequest),
    /// `detach-client`
    DetachClient(DetachClientRequest),
    /// `set-option`
    SetOption(SetOptionRequest),
    /// `set-environment`
    SetEnvironment(SetEnvironmentRequest),
    /// `set-hook`
    SetHook(SetHookRequest),
    /// `next-layout`
    NextLayout(NextLayoutRequest),
    /// `previous-layout`
    PreviousLayout(PreviousLayoutRequest),
    /// `show-options`
    ShowOptions(ShowOptionsRequest),
    /// `show-environment`
    ShowEnvironment(ShowEnvironmentRequest),
    /// `set-buffer`
    SetBuffer(SetBufferRequest),
    /// `show-buffer`
    ShowBuffer(ShowBufferRequest),
    /// `paste-buffer`
    PasteBuffer(PasteBufferRequest),
    /// `list-buffers`
    ListBuffers(ListBuffersRequest),
    /// `delete-buffer`
    DeleteBuffer(DeleteBufferRequest),
    /// `load-buffer`
    LoadBuffer(LoadBufferRequest),
    /// `save-buffer`
    SaveBuffer(SaveBufferRequest),
    /// `capture-pane`
    CapturePane(CapturePaneRequest),
    /// `display-message`
    DisplayMessage(DisplayMessageRequest),
    /// `run-shell`
    RunShell(RunShellRequest),
    /// `if-shell`
    IfShell(IfShellRequest),
    /// `wait-for`
    WaitFor(WaitForRequest),
    /// `rename-session`
    RenameSession(RenameSessionRequest),
    /// `list-sessions`
    ListSessions(ListSessionsRequest),
    /// `list-panes`
    ListPanes(ListPanesRequest),
    /// `source-file`
    SourceFile(SourceFileRequest),
    /// `set-option` using an open string-based option name.
    SetOptionByName(SetOptionByNameRequest),
    /// Extended `set-hook` mutation semantics.
    SetHookMutation(SetHookMutationRequest),
    /// `show-hooks`
    ShowHooks(ShowHooksRequest),
    /// Extended `send-keys` semantics including key-table dispatch and format expansion.
    SendKeysExt(SendKeysExtRequest),
    /// Extended `switch-client` semantics including `-T key-table`.
    SwitchClientExt(SwitchClientExtRequest),
    /// `bind-key`
    BindKey(BindKeyRequest),
    /// `unbind-key`
    UnbindKey(UnbindKeyRequest),
    /// `list-keys`
    ListKeys(ListKeysRequest),
    /// `send-prefix`
    SendPrefix(SendPrefixRequest),
    /// `clear-history`
    ClearHistory(ClearHistoryRequest),
    /// `copy-mode`
    CopyMode(CopyModeRequest),
    /// Internal detached upgrade into tmux-compatible control mode.
    ControlMode(ControlModeRequest),
    /// `clock-mode`
    ClockMode(ClockModeRequest),
    /// `show-messages`
    ShowMessages(ShowMessagesRequest),
    /// Extended `new-session` semantics including grouped sessions and attach-if-exists.
    NewSessionExt(NewSessionExtRequest),
    /// Extended `attach-session` semantics including client flags and detach-others.
    AttachSessionExt(AttachSessionExtRequest),
    /// Extended `switch-client` semantics including `-l`, `-n`, `-p`, and readonly toggles.
    SwitchClientExt2(SwitchClientExt2Request),
    /// `select-layout` with a tmux custom layout string.
    SelectCustomLayout(SelectCustomLayoutRequest),
    /// `select-layout -o`
    SelectOldLayout(SelectOldLayoutRequest),
    /// `select-layout -E`
    SpreadLayout(SpreadLayoutRequest),
    /// `kill-server`
    KillServer(KillServerRequest),
    /// `lock-server`
    LockServer(LockServerRequest),
    /// `lock-session`
    LockSession(LockSessionRequest),
    /// `lock-client`
    LockClient(LockClientRequest),
    /// `server-access`
    ServerAccess(ServerAccessRequest),
    /// `refresh-client`
    RefreshClient(RefreshClientRequest),
    /// `list-clients`
    ListClients(ListClientsRequest),
    /// `suspend-client`
    SuspendClient(SuspendClientRequest),
    /// Extended `detach-client` semantics including `-a`, `-s`, `-P`, and `-E`.
    DetachClientExt(DetachClientExtRequest),
    /// Further-extended `attach-session` semantics including `-c working-directory`.
    AttachSessionExt2(AttachSessionExt2Request),
    /// Further-extended `switch-client` semantics including `-c target-client` and `-Z`.
    SwitchClientExt3(SwitchClientExt3Request),
    /// `resize-window`
    ResizeWindow(ResizeWindowRequest),
    /// `respawn-window`
    RespawnWindow(RespawnWindowRequest),
    /// `move-pane`
    MovePane(MovePaneRequest),
    /// `pipe-pane`
    PipePane(PipePaneRequest),
    /// `respawn-pane`
    RespawnPane(RespawnPaneRequest),
    /// `link-window`
    LinkWindow(LinkWindowRequest),
    /// `unlink-window`
    UnlinkWindow(UnlinkWindowRequest),
    /// `select-pane -m` / `select-pane -M`
    SelectPaneMark(SelectPaneMarkRequest),
    /// Internal detached target resolution for tmux-style raw target text.
    ResolveTarget(ResolveTargetRequest),
    /// Extended `split-window` semantics including an explicit shell command.
    SplitWindowExt(SplitWindowExtRequest),
    /// Internal SDK/daemon version and capability negotiation.
    Handshake(HandshakeRequest),
    /// Internal daemon-backed structured pane snapshot endpoint.
    PaneSnapshot(PaneSnapshotRequest),
    /// Internal daemon-backed pane output subscription endpoint.
    SubscribePaneOutput(SubscribePaneOutputRequest),
    /// Internal daemon-backed pane output unsubscription endpoint.
    UnsubscribePaneOutput(UnsubscribePaneOutputRequest),
    /// Internal daemon-backed pane output cursor polling endpoint.
    PaneOutputCursor(PaneOutputCursorRequest),
    /// Internal daemon-backed SDK byte wait endpoint.
    SdkWaitForOutput(SdkWaitForOutputRequest),
    /// Internal daemon-backed SDK wait cancellation endpoint.
    CancelSdkWait(CancelSdkWaitRequest),
    /// SDK pane input endpoint with stable pane-id targeting.
    PaneInput(PaneInputRequest),
    /// SDK pane resize endpoint with stable pane-id targeting.
    PaneResize(PaneResizeRequest),
    /// SDK pane kill endpoint with stable pane-id targeting.
    PaneKill(PaneKillRequest),
    /// SDK pane respawn endpoint with stable pane-id targeting.
    PaneRespawn(PaneRespawnRequest),
    /// SDK pane snapshot endpoint with stable pane-id targeting.
    PaneSnapshotRef(PaneSnapshotRefRequest),
    /// SDK pane select/title endpoint with stable pane-id targeting.
    PaneSelect(PaneSelectRequest),
    /// SDK pane input broadcast endpoint with stable pane-id targeting.
    PaneBroadcastInput(PaneBroadcastInputRequest),
    /// SDK app-owned session lease create endpoint.
    CreateSessionLease(CreateSessionLeaseRequest),
    /// SDK app-owned session lease renewal endpoint.
    RenewSessionLease(RenewSessionLeaseRequest),
    /// SDK app-owned session lease release endpoint.
    ReleaseSessionLease(ReleaseSessionLeaseRequest),
    /// Internal daemon-backed pane output subscription endpoint with stable pane-id targeting.
    SubscribePaneOutputRef(SubscribePaneOutputRefRequest),
    /// Internal daemon-backed SDK byte wait endpoint with stable pane-id targeting.
    SdkWaitForOutputRef(SdkWaitForOutputRefRequest),
}

impl Request {
    /// Returns the stable routing name for the request variant.
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
            Self::LinkWindow(_) => "link-window",
            Self::MoveWindow(_) => "move-window",
            Self::SwapWindow(_) => "swap-window",
            Self::RotateWindow(_) => "rotate-window",
            Self::SplitWindow(_) | Self::SplitWindowExt(_) => "split-window",
            Self::SwapPane(_) => "swap-pane",
            Self::LastPane(_) => "last-pane",
            Self::JoinPane(_) => "join-pane",
            Self::BreakPane(_) => "break-pane",
            Self::KillPane(_) => "kill-pane",
            Self::SelectLayout(_) => "select-layout",
            Self::ResizePane(_) => "resize-pane",
            Self::DisplayPanes(_) => "display-panes",
            Self::SelectPane(_) | Self::SelectPaneAdjacent(_) | Self::SelectPaneMark(_) => {
                "select-pane"
            }
            Self::SendKeys(_) => "send-keys",
            Self::AttachSession(_) => "attach-session",
            Self::SwitchClient(_) => "switch-client",
            Self::DetachClient(_) => "detach-client",
            Self::SetOption(_) => "set-option",
            Self::SetEnvironment(_) => "set-environment",
            Self::SetHook(_) => "set-hook",
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
            Self::PaneSnapshot(_) => "pane-snapshot",
            Self::SubscribePaneOutput(_) | Self::SubscribePaneOutputRef(_) => {
                "subscribe-pane-output"
            }
            Self::UnsubscribePaneOutput(_) => "unsubscribe-pane-output",
            Self::PaneOutputCursor(_) => "pane-output-cursor",
            Self::SdkWaitForOutput(_) | Self::SdkWaitForOutputRef(_) => "sdk-wait-output",
            Self::CancelSdkWait(_) => "cancel-sdk-wait",
            Self::PaneInput(_) => "send-keys",
            Self::PaneBroadcastInput(_) => "send-keys",
            Self::CreateSessionLease(_) => "create-session-lease",
            Self::RenewSessionLease(_) => "renew-session-lease",
            Self::ReleaseSessionLease(_) => "release-session-lease",
            Self::PaneResize(_) => "resize-pane",
            Self::PaneKill(_) => "kill-pane",
            Self::PaneRespawn(_) => "respawn-pane",
            Self::PaneSnapshotRef(_) => "pane-snapshot",
            Self::PaneSelect(_) => "select-pane",
            Self::DisplayMessage(_) => "display-message",
            Self::ResolveTarget(_) => "resolve-target",
            Self::RunShell(_) => "run-shell",
            Self::IfShell(_) => "if-shell",
            Self::WaitFor(_) => "wait-for",
            Self::RenameSession(_) => "rename-session",
            Self::ListSessions(_) => "list-sessions",
            Self::ListPanes(_) => "list-panes",
            Self::SourceFile(_) => "source-file",
            Self::UnlinkWindow(_) => "unlink-window",
            Self::SetOptionByName(_) => "set-option",
            Self::SetHookMutation(_) => "set-hook",
            Self::ShowHooks(_) => "show-hooks",
            Self::SendKeysExt(_) => "send-keys",
            Self::SwitchClientExt(_) => "switch-client",
            Self::BindKey(_) => "bind-key",
            Self::UnbindKey(_) => "unbind-key",
            Self::ListKeys(_) => "list-keys",
            Self::SendPrefix(_) => "send-prefix",
            Self::ClearHistory(_) => "clear-history",
            Self::CopyMode(_) => "copy-mode",
            Self::ControlMode(_) => "control-mode",
            Self::ClockMode(_) => "clock-mode",
            Self::ShowMessages(_) => "show-messages",
            Self::NewSessionExt(_) => "new-session",
            Self::AttachSessionExt(_) => "attach-session",
            Self::SwitchClientExt2(_) => "switch-client",
            Self::SelectCustomLayout(_) => "select-layout",
            Self::SelectOldLayout(_) => "select-layout",
            Self::SpreadLayout(_) => "select-layout",
            Self::ResizeWindow(_) => "resize-window",
            Self::RespawnWindow(_) => "respawn-window",
            Self::MovePane(_) => "move-pane",
            Self::PipePane(_) => "pipe-pane",
            Self::RespawnPane(_) => "respawn-pane",
            Self::KillServer(_) => "kill-server",
            Self::LockServer(_) => "lock-server",
            Self::LockSession(_) => "lock-session",
            Self::LockClient(_) => "lock-client",
            Self::ServerAccess(_) => "server-access",
            Self::RefreshClient(_) => "refresh-client",
            Self::ListClients(_) => "list-clients",
            Self::SuspendClient(_) => "suspend-client",
            Self::DetachClientExt(_) => "detach-client",
            Self::AttachSessionExt2(_) => "attach-session",
            Self::SwitchClientExt3(_) => "switch-client",
            Self::Handshake(_) => "handshake",
        }
    }
}

/// Request payload for `display-message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayMessageRequest {
    /// The optional exact session, window, or pane target used as format context.
    pub target: Option<Target>,
    /// Whether to print the expanded message to stdout instead of displaying it.
    pub print: bool,
    /// The optional format string. When omitted, the tmux-compatible default is used.
    pub message: Option<String>,
}

/// Request payload for `show-messages`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowMessagesRequest {
    /// Whether to print the server job summary.
    pub jobs: bool,
    /// Whether to print terminal information.
    pub terminals: bool,
    /// The optional target client filter used by terminal and job summaries.
    pub target_client: Option<String>,
}

/// Request payload for `run-shell`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunShellRequest {
    /// The server-local shell command passed to `sh -c`.
    pub command: String,
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

/// Request payload for `source-file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFileRequest {
    /// The path arguments to expand, glob, parse, and optionally execute.
    pub paths: Vec<String>,
    /// Whether missing files and glob misses should be suppressed.
    pub quiet: bool,
    /// Whether to parse only without executing parsed commands.
    pub parse_only: bool,
    /// Whether parsed commands should be printed to stdout.
    pub verbose: bool,
    /// Whether each path argument should be format-expanded before globbing.
    pub expand_paths: bool,
    /// Optional pane target used as the `-F` format context.
    pub target: Option<PaneTarget>,
    /// The caller working directory used to resolve relative paths.
    pub caller_cwd: Option<PathBuf>,
    /// Content read from client stdin for `source-file -`.
    pub stdin: Option<String>,
}

/// Request payload for `unlink-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlinkWindowRequest {
    /// The window slot to remove.
    pub target: WindowTarget,
    /// Whether removing the final link is allowed (`-k`).
    #[serde(default)]
    pub kill_if_last: bool,
}

/// The supported `wait-for` operation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitForMode {
    /// Wait for the next signal on the named channel.
    Wait,
    /// Signal all current plain waiters on the named channel.
    Signal,
    /// Acquire the named server-local lock, waiting in FIFO order when held.
    Lock,
    /// Release the named server-local lock.
    Unlock,
}

/// Request payload for `wait-for`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForRequest {
    /// The server-local wait channel name.
    pub channel: String,
    /// The selected wait operation.
    pub mode: WaitForMode,
}

/// Request payload for a daemon-backed SDK byte wait.
///
/// This is intentionally distinct from tmux-compatible [`WaitForRequest`].
/// SDK waits are pane-output waits with typed IDs used only for cancellation
/// and teardown bookkeeping; they never signal or lock tmux `wait-for`
/// channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkWaitForOutputRequest {
    /// Opaque SDK transport owner for this wait.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
    /// Pane whose raw output stream is observed.
    pub target: PaneTarget,
    /// Raw byte sequence to search for in pane output.
    pub bytes: Vec<u8>,
    /// Cursor position used when arming the wait.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for a daemon-backed SDK byte wait by slot or stable id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkWaitForOutputRefRequest {
    /// Opaque SDK transport owner for this wait.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
    /// Pane whose raw output stream is observed.
    pub target: PaneTargetRef,
    /// Raw byte sequence to search for in pane output.
    pub bytes: Vec<u8>,
    /// Cursor position used when arming the wait.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for best-effort cancellation of a daemon-backed SDK wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelSdkWaitRequest {
    /// Opaque SDK transport owner for the wait being cancelled.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
}

/// Request payload for `list-panes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPanesRequest {
    /// The exact target session name.
    pub target: SessionName,
    /// Optional exact target window index.
    #[serde(default)]
    pub target_window_index: Option<u32>,
    /// An optional server-side format template.
    pub format: Option<String>,
}

#[cfg(test)]
#[path = "request/compat_tests.rs"]
mod compat_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OptionScopeSelector, PaneTarget, ScopeSelector, SelectPaneDirection, SetOptionMode,
        SplitDirection,
    };

    fn alpha() -> SessionName {
        SessionName::new("alpha").expect("valid session")
    }

    fn pane() -> PaneTarget {
        PaneTarget::new(alpha(), 0)
    }

    #[test]
    fn request_command_names_cover_extended_aliases_and_internal_tags() {
        assert_eq!(
            Request::NewSessionExt(NewSessionExtRequest {
                session_name: None,
                working_directory: None,
                detached: true,
                size: None,
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: None,
                process_command: None,
                passthrough: false,
            })
            .command_name(),
            "new-session"
        );
        assert_eq!(
            Request::SplitWindowExt(SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(pane()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
            })
            .command_name(),
            "split-window"
        );
        assert_eq!(
            Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
                target: pane(),
                direction: SelectPaneDirection::Right,
            })
            .command_name(),
            "select-pane"
        );
        assert_eq!(
            Request::SelectPaneMark(SelectPaneMarkRequest {
                target: pane(),
                clear: false,
                title: None,
            })
            .command_name(),
            "select-pane"
        );
        assert_eq!(
            Request::SetOptionByName(SetOptionByNameRequest {
                scope: OptionScopeSelector::ServerGlobal,
                name: "status".to_owned(),
                value: Some("on".to_owned()),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
            })
            .command_name(),
            "set-option"
        );
        assert_eq!(
            Request::SetHookMutation(SetHookMutationRequest {
                scope: ScopeSelector::Global,
                hook: crate::HookName::AfterNewSession,
                command: None,
                lifecycle: crate::HookLifecycle::Persistent,
                append: false,
                unset: true,
                run_immediately: false,
                index: None,
            })
            .command_name(),
            "set-hook"
        );
        assert_eq!(
            Request::AttachSessionExt2(AttachSessionExt2Request {
                target: Some(alpha()),
                target_spec: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
                working_directory: None,
                client_terminal: crate::ClientTerminalContext::default(),
                client_size: None,
            })
            .command_name(),
            "attach-session"
        );
        assert_eq!(
            Request::SwitchClientExt3(SwitchClientExt3Request {
                target_client: None,
                target: Some("alpha:0.0".to_owned()),
                key_table: None,
                last_session: false,
                next_session: false,
                previous_session: false,
                toggle_read_only: false,
                sort_order: None,
                skip_environment_update: false,
                zoom: false,
            })
            .command_name(),
            "switch-client"
        );
        assert_eq!(
            Request::ResolveTarget(ResolveTargetRequest {
                target: Some("alpha:0.0".to_owned()),
                target_type: ResolveTargetType::Pane,
                window_index: false,
                prefer_unattached: false,
            })
            .command_name(),
            "resolve-target"
        );
        assert_eq!(
            Request::Handshake(HandshakeRequest::current()).command_name(),
            "handshake"
        );
        assert_eq!(
            Request::SdkWaitForOutput(SdkWaitForOutputRequest {
                owner_id: SdkWaitOwnerId::new(1),
                wait_id: SdkWaitId::new(1),
                target: pane(),
                bytes: b"ready".to_vec(),
                start: PaneOutputSubscriptionStart::Now,
            })
            .command_name(),
            "sdk-wait-output"
        );
        assert_eq!(
            Request::CancelSdkWait(CancelSdkWaitRequest {
                owner_id: SdkWaitOwnerId::new(1),
                wait_id: SdkWaitId::new(1),
            })
            .command_name(),
            "cancel-sdk-wait"
        );
        assert_eq!(
            Request::WaitFor(WaitForRequest {
                channel: "ready".to_owned(),
                mode: WaitForMode::Wait,
            })
            .command_name(),
            "wait-for"
        );
    }
}
