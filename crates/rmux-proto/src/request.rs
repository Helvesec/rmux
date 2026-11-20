//! Detached request contracts.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{ControlModeRequest, HandshakeRequest, PaneTarget, SessionName, WindowTarget};
#[cfg(test)]
use crate::{SdkWaitId, SdkWaitOwnerId};

mod compat;

mod display;
pub use display::{
    DisplayMessageDurationMillis, DisplayMessageDurationParseError, DisplayMessageExtRequest,
    DisplayMessageRequest,
};

mod show;
pub use show::ShowHooksRequest;
pub use show::{ShowEnvironmentRequest, ShowOptionsRequest};

mod layout;
pub use layout::{
    decode_rmux_window_snapshot_json, decode_rmux_window_snapshot_v2_json,
    ExportWindowSnapshotRequest, ExportWindowSnapshotV2Request, NextLayoutRequest,
    PreviousLayoutRequest, ResizePaneAxesRequest, RestoreWindowSnapshotRequest,
    RestoreWindowSnapshotV2Request, RmuxFloatingPaneSnapshot, RmuxPaneRuntimeIdentity,
    RmuxWindowLayoutMetadata, RmuxWindowResizeTopology, RmuxWindowSnapshot, RmuxWindowSnapshotCell,
    RmuxWindowSnapshotSplitDirection, RmuxWindowSnapshotV2, SelectCustomLayoutRequest,
    SelectLayoutRequest, SelectLayoutTarget, SelectOldLayoutRequest, SpreadLayoutRequest,
    RMUX_WINDOW_SNAPSHOT_V2_VERSION, RMUX_WINDOW_SNAPSHOT_VERSION,
};

mod pane;
pub use pane::{
    BreakPaneRequest, DisplayPanesRequest, JoinPaneRequest, KillPaneRequest, LastPaneRequest,
    MovePaneRequest, PaneBroadcastInputRequest, PaneForegroundStateRequest, PaneInputRequest,
    PaneKillRequest, PaneOptionGetRequest, PaneOptionSetRequest, PaneOutputCursorRequest,
    PaneOutputSubscriptionStart, PaneResizeRequest, PaneRespawnRequest, PaneSelectRequest,
    PaneSnapshotRefRequest, PaneSnapshotRequest, PaneSplitSize, PaneStateCursorRequest,
    PaneStreamCursorRequest, PaneStreamMode, PipePaneRequest, ResizePaneRequest,
    ResizePaneTargetActionRequest, RespawnPaneRequest, SelectPaneAdjacentRequest,
    SelectPaneDirection, SelectPaneMarkRequest, SelectPaneRequest, SendKeysExt2Request,
    SendKeysExtRequest, SendKeysRequest, SplitWindowExtRequest, SplitWindowIdentityRequest,
    SplitWindowRequest, SplitWindowTarget, SplitWindowTargetActionRequest,
    SubscribePaneOutputRefRequest, SubscribePaneOutputRequest, SubscribePaneStateRequest,
    SubscribePaneStreamRequest, SwapPaneDirection, SwapPaneRequest, UnsubscribePaneOutputRequest,
    UnsubscribePaneStateRequest, UnsubscribePaneStreamRequest,
};

mod window;
pub use window::{
    KillWindowRequest, LastWindowRequest, LinkWindowRequest, ListWindowsRequest, MoveWindowRequest,
    MoveWindowTarget, NewWindowRequest, NextWindowRequest, PreviousWindowRequest,
    RenameWindowRequest, ResizeWindowAdjustment, ResizeWindowRequest, RespawnWindowRequest,
    RotateWindowDirection, RotateWindowRequest, SelectWindowRequest, SwapWindowRequest,
};

mod target;
pub use target::{ResolveTargetRequest, ResolveTargetType};

mod session;
pub use session::{
    CreateSessionLeaseRequest, HasSessionRequest, KillSessionRequest, ListSessionsRequest,
    NewSessionExtRequest, NewSessionRequest, ReleaseSessionLeaseRequest, RenameSessionRequest,
    RenewSessionLeaseRequest,
};

mod server;
pub use server::{
    DaemonStatusRequest, KillServerRequest, LockClientRequest, LockServerRequest,
    LockSessionRequest, ServerAccessRequest, ShutdownIfIdleRequest,
};

mod client;
pub use client::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
    AttachSessionRequest, DetachClientExtRequest, DetachClientRequest, ListClientsRequest,
    RefreshClientRequest, SuspendClientRequest, SwitchClientExt2Request, SwitchClientExt3Request,
    SwitchClientExtRequest, SwitchClientRequest,
};

mod keys;
pub use keys::{
    BindKeyRequest, ClockModeRequest, CopyModeRequest, ListKeysRequest, SendPrefixRequest,
    UnbindKeyRequest,
};

mod options;
pub use options::{
    SetEnvironmentMode, SetEnvironmentRequest, SetHookMutationRequest, SetHookRequest,
    SetOptionByNameRequest, SetOptionRequest,
};

mod buffer;
pub use buffer::{
    CapturePaneRequest, CapturePaneTargetActionRequest, ClearHistoryRequest, DeleteBufferRequest,
    ListBuffersRequest, LoadBufferRequest, PasteBufferRequest, SaveBufferRequest, SetBufferRequest,
    ShowBufferRequest,
};

mod web;
pub use web::{
    CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest, StopAllWebSharesRequest,
    StopWebShareRequest, WebShareConfigRequest, WebShareRequest, WebShareScope, WebShareUrlOptions,
    WebTerminalPalette, WebTerminalTheme,
};

mod shell;
pub use shell::{IfShellRequest, RunShellDelaySeconds, RunShellRequest};

mod routing;

mod wait;
pub use wait::{
    CancelSdkWaitRequest, SdkWaitForOutputRefRequest, SdkWaitForOutputRequest, WaitForMode,
    WaitForRequest,
};

/// All detached public command and internal RPC requests supported by the wire
/// protocol.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// `new-session`
    NewSession(NewSessionRequest),
    /// `has-session`
    HasSession(HasSessionRequest),
    /// `kill-session`
    KillSession(KillSessionRequest),
    /// `new-window`
    NewWindow(Box<NewWindowRequest>),
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
    ListWindows(Box<ListWindowsRequest>),
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
    BreakPane(Box<BreakPaneRequest>),
    /// `kill-pane`
    KillPane(KillPaneRequest),
    /// `select-layout`
    SelectLayout(SelectLayoutRequest),
    /// `resize-pane`
    ResizePane(ResizePaneRequest),
    /// `display-panes`
    DisplayPanes(Box<DisplayPanesRequest>),
    /// `select-pane`
    SelectPane(Box<SelectPaneRequest>),
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
    SetEnvironment(Box<SetEnvironmentRequest>),
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
    SetBuffer(Box<SetBufferRequest>),
    /// `show-buffer`
    ShowBuffer(ShowBufferRequest),
    /// `paste-buffer`
    PasteBuffer(Box<PasteBufferRequest>),
    /// `list-buffers`
    ListBuffers(ListBuffersRequest),
    /// `delete-buffer`
    DeleteBuffer(DeleteBufferRequest),
    /// `load-buffer`
    LoadBuffer(Box<LoadBufferRequest>),
    /// `save-buffer`
    SaveBuffer(SaveBufferRequest),
    /// `capture-pane`
    CapturePane(Box<CapturePaneRequest>),
    /// `display-message`
    DisplayMessage(DisplayMessageRequest),
    /// `run-shell`
    RunShell(Box<RunShellRequest>),
    /// `if-shell`
    IfShell(Box<IfShellRequest>),
    /// `wait-for`
    WaitFor(WaitForRequest),
    /// `rename-session`
    RenameSession(RenameSessionRequest),
    /// `list-sessions`
    ListSessions(ListSessionsRequest),
    /// `list-panes`
    ListPanes(Box<ListPanesRequest>),
    /// `source-file`
    SourceFile(Box<SourceFileRequest>),
    /// `set-option` using an open string-based option name.
    SetOptionByName(Box<SetOptionByNameRequest>),
    /// Extended `set-hook` mutation semantics.
    SetHookMutation(SetHookMutationRequest),
    /// `show-hooks`
    ShowHooks(ShowHooksRequest),
    /// Extended `send-keys` semantics including key-table dispatch and format expansion.
    SendKeysExt(SendKeysExtRequest),
    /// Extended `switch-client` semantics including `-T key-table`.
    SwitchClientExt(SwitchClientExtRequest),
    /// `bind-key`
    BindKey(Box<BindKeyRequest>),
    /// `unbind-key`
    UnbindKey(UnbindKeyRequest),
    /// `list-keys`
    ListKeys(Box<ListKeysRequest>),
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
    NewSessionExt(Box<NewSessionExtRequest>),
    /// Extended `attach-session` semantics including client flags and detach-others.
    AttachSessionExt(AttachSessionExtRequest),
    /// Extended `switch-client` semantics including `-l`, `-n`, `-p`, and readonly toggles.
    SwitchClientExt2(Box<SwitchClientExt2Request>),
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
    RefreshClient(Box<RefreshClientRequest>),
    /// `list-clients`
    ListClients(Box<ListClientsRequest>),
    /// `suspend-client`
    SuspendClient(SuspendClientRequest),
    /// Extended `detach-client` semantics including `-a`, `-s`, `-P`, and `-E`.
    DetachClientExt(DetachClientExtRequest),
    /// Further-extended `attach-session` semantics including `-c working-directory`.
    AttachSessionExt2(Box<AttachSessionExt2Request>),
    /// Further-extended `switch-client` semantics including `-c target-client` and `-Z`.
    SwitchClientExt3(Box<SwitchClientExt3Request>),
    /// `resize-window`
    ResizeWindow(ResizeWindowRequest),
    /// `respawn-window`
    RespawnWindow(Box<RespawnWindowRequest>),
    /// `move-pane`
    MovePane(MovePaneRequest),
    /// `pipe-pane`
    PipePane(PipePaneRequest),
    /// `respawn-pane`
    RespawnPane(Box<RespawnPaneRequest>),
    /// `link-window`
    LinkWindow(LinkWindowRequest),
    /// `unlink-window`
    UnlinkWindow(UnlinkWindowRequest),
    /// `select-pane -m` / `select-pane -M`
    SelectPaneMark(SelectPaneMarkRequest),
    /// Internal detached target resolution for tmux-style raw target text.
    ResolveTarget(ResolveTargetRequest),
    /// Extended `split-window` semantics including an explicit shell command.
    SplitWindowExt(Box<SplitWindowExtRequest>),
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
    PaneRespawn(Box<PaneRespawnRequest>),
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
    /// Internal daemon version and activity status endpoint.
    DaemonStatus(DaemonStatusRequest),
    /// Internal idle-only shutdown endpoint used by seamless upgrades.
    ShutdownIfIdle(ShutdownIfIdleRequest),
    /// Browser-visible pane sharing command family.
    WebShare(Box<WebShareRequest>),
    /// `display-message` extension with target-client context.
    DisplayMessageExt(Box<DisplayMessageExtRequest>),
    /// `send-keys` extension with target-client context.
    SendKeysExt2(Box<SendKeysExt2Request>),
    /// Attach-session extension with attach-stream client capabilities.
    AttachSessionExt3(Box<AttachSessionExt3Request>),
    /// `split-window` with raw target text resolved server-side.
    SplitWindowTargetAction(Box<SplitWindowTargetActionRequest>),
    /// `resize-pane` with raw target text resolved server-side.
    ResizePaneTargetAction(ResizePaneTargetActionRequest),
    /// `capture-pane` with raw target text resolved server-side.
    CapturePaneTargetAction(Box<CapturePaneTargetActionRequest>),
    /// SDK pane option mutation endpoint with stable pane-id targeting.
    PaneOptionSet(PaneOptionSetRequest),
    /// SDK pane option lookup endpoint with stable pane-id targeting.
    PaneOptionGet(PaneOptionGetRequest),
    /// SDK pane-state subscription endpoint with stable pane-id targeting.
    SubscribePaneState(SubscribePaneStateRequest),
    /// SDK pane-state cursor endpoint.
    PaneStateCursor(PaneStateCursorRequest),
    /// SDK pane-state unsubscription endpoint.
    UnsubscribePaneState(UnsubscribePaneStateRequest),
    /// SDK pane foreground-state endpoint with stable pane-id targeting.
    PaneForegroundState(PaneForegroundStateRequest),
    /// SDK split endpoint returning visible and stable pane identity atomically.
    SplitWindowIdentity(Box<SplitWindowIdentityRequest>),
    /// Opens a recoverable raw or surface pane stream.
    SubscribePaneStream(SubscribePaneStreamRequest),
    /// Polls a recoverable raw or surface pane stream.
    PaneStreamCursor(PaneStreamCursorRequest),
    /// Closes a recoverable raw or surface pane stream.
    UnsubscribePaneStream(UnsubscribePaneStreamRequest),
    /// Atomic capability negotiation and server-owned CLI argv execution.
    RuntimeCommandExecution(Box<crate::RuntimeCommandExecutionRequest>),
    /// Versioned atomic split/key command and binary terminal payload offer.
    SplitWindowCommandV1(Box<crate::SplitWindowCommandV1Request>),
    /// Explicit typed RMUX window snapshot export.
    ExportWindowSnapshot(ExportWindowSnapshotRequest),
    /// Explicit typed RMUX window snapshot restore.
    RestoreWindowSnapshot(Box<RestoreWindowSnapshotRequest>),
    /// Atomic runtime execution with support and declared operation separated.
    RuntimeCommandExecutionV2(Box<crate::RuntimeCommandExecutionV2Request>),
    /// One `new-pane` offered with the finished bytes it is to display.
    NewPaneInputV1(Box<crate::NewPaneInputV1Request>),
    /// Opt-in canonical V2 snapshot export.
    ExportWindowSnapshotV2(ExportWindowSnapshotV2Request),
    /// Transactional canonical V2 snapshot restore.
    RestoreWindowSnapshotV2(Box<RestoreWindowSnapshotV2Request>),
    /// Sequential absolute axes with deferred height validation.
    ResizePaneAxes(Box<ResizePaneAxesRequest>),
    /// File-backed source execution with separate finished binary input.
    SourceFileInputV1(Box<crate::SourceFileInputV1Request>),
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
    /// Optional filter expression.
    #[serde(default)]
    pub filter: Option<String>,
    /// Optional sort-order token.
    #[serde(default)]
    pub sort_order: Option<String>,
    /// Whether the selected sort order should be reversed.
    #[serde(default)]
    pub reversed: bool,
}

#[cfg(test)]
#[path = "request/compat_tests.rs"]
mod compat_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        OptionScopeSelector, PaneTarget, ScopeSelector, SelectPaneDirection, SetOptionMode,
        SplitDirection, WebShareScope,
    };
    use serde::Serialize;
    use std::mem::size_of;

    fn alpha() -> SessionName {
        SessionName::new("alpha").expect("valid session")
    }

    fn pane() -> PaneTarget {
        PaneTarget::new(alpha(), 0)
    }

    #[test]
    fn request_command_names_cover_extended_aliases_and_internal_tags() {
        assert_eq!(
            Request::NewSessionExt(Box::new(NewSessionExtRequest {
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
                client_environment: None,
                skip_environment_update: false,
            }))
            .command_name(),
            "new-session"
        );
        assert_eq!(
            Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                target: SplitWindowTarget::Pane(pane()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            }))
            .command_name(),
            "split-window"
        );
        assert_eq!(
            Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
                target: pane(),
                direction: SelectPaneDirection::Right,
                preserve_zoom: false,
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
            Request::SetOptionByName(Box::new(SetOptionByNameRequest {
                scope: OptionScopeSelector::ServerGlobal,
                name: "status".to_owned(),
                value: Some("on".to_owned()),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                format: false,
                format_target: None,
            }))
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
            Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
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
            }))
            .command_name(),
            "attach-session"
        );
        assert_eq!(
            Request::AttachSessionExt3(Box::new(AttachSessionExt3Request::from_ext2(
                AttachSessionExt2Request {
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
                },
                vec![crate::CAPABILITY_ATTACH_RENDER.to_owned()],
            )))
            .command_name(),
            "attach-session"
        );
        assert_eq!(
            Request::SwitchClientExt3(Box::new(SwitchClientExt3Request {
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
            }))
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

    #[test]
    fn pr6g_request_boxing_keeps_request_size_bounded() {
        let request_size = size_of::<Request>();
        let box_size = size_of::<Box<NewSessionExtRequest>>();

        eprintln!("PR6G size baseline: Request={request_size} Box<T>={box_size}");
        eprintln!(
            "PR6G candidate sizes: NewSessionExt={} NewWindow={} SetOptionByName={} \
             AttachSessionExt3={} SwitchClientExt3={} SendKeysExt2={} SourceFile={} WebShare={}",
            size_of::<NewSessionExtRequest>(),
            size_of::<NewWindowRequest>(),
            size_of::<SetOptionByNameRequest>(),
            size_of::<AttachSessionExt3Request>(),
            size_of::<SwitchClientExt3Request>(),
            size_of::<SendKeysExt2Request>(),
            size_of::<SourceFileRequest>(),
            size_of::<WebShareRequest>(),
        );
        let mut remaining = [
            ("AttachSessionExt2", size_of::<AttachSessionExt2Request>()),
            ("BindKey", size_of::<BindKeyRequest>()),
            ("BreakPane", size_of::<BreakPaneRequest>()),
            ("CapturePane", size_of::<CapturePaneRequest>()),
            ("ClearHistory", size_of::<ClearHistoryRequest>()),
            ("ClockMode", size_of::<ClockModeRequest>()),
            ("ControlMode", size_of::<ControlModeRequest>()),
            ("CopyMode", size_of::<CopyModeRequest>()),
            ("CreateSessionLease", size_of::<CreateSessionLeaseRequest>()),
            ("DeleteBuffer", size_of::<DeleteBufferRequest>()),
            ("DetachClientExt", size_of::<DetachClientExtRequest>()),
            ("DisplayPanes", size_of::<DisplayPanesRequest>()),
            ("DisplayMessage", size_of::<DisplayMessageRequest>()),
            ("DisplayMessageExt", size_of::<DisplayMessageExtRequest>()),
            ("Handshake", size_of::<HandshakeRequest>()),
            ("IfShell", size_of::<IfShellRequest>()),
            ("JoinPane", size_of::<JoinPaneRequest>()),
            ("KillPane", size_of::<KillPaneRequest>()),
            ("KillWindow", size_of::<KillWindowRequest>()),
            ("LinkWindow", size_of::<LinkWindowRequest>()),
            ("ListClients", size_of::<ListClientsRequest>()),
            ("ListKeys", size_of::<ListKeysRequest>()),
            ("ListPanes", size_of::<ListPanesRequest>()),
            ("ListSessions", size_of::<ListSessionsRequest>()),
            ("ListWindows", size_of::<ListWindowsRequest>()),
            ("LoadBuffer", size_of::<LoadBufferRequest>()),
            ("MovePane", size_of::<MovePaneRequest>()),
            ("MoveWindow", size_of::<MoveWindowRequest>()),
            ("NewSession", size_of::<NewSessionRequest>()),
            ("PasteBuffer", size_of::<PasteBufferRequest>()),
            ("PaneInput", size_of::<PaneInputRequest>()),
            ("PaneKill", size_of::<PaneKillRequest>()),
            ("PaneResize", size_of::<PaneResizeRequest>()),
            ("PaneRespawn", size_of::<PaneRespawnRequest>()),
            ("PaneSelect", size_of::<PaneSelectRequest>()),
            ("PaneSnapshot", size_of::<PaneSnapshotRequest>()),
            ("PaneSnapshotRef", size_of::<PaneSnapshotRefRequest>()),
            ("PipePane", size_of::<PipePaneRequest>()),
            ("RefreshClient", size_of::<RefreshClientRequest>()),
            ("RespawnPane", size_of::<RespawnPaneRequest>()),
            ("RespawnWindow", size_of::<RespawnWindowRequest>()),
            ("ResizePane", size_of::<ResizePaneRequest>()),
            ("ResizeWindow", size_of::<ResizeWindowRequest>()),
            ("RotateWindow", size_of::<RotateWindowRequest>()),
            ("RunShell", size_of::<RunShellRequest>()),
            ("SaveBuffer", size_of::<SaveBufferRequest>()),
            ("SelectCustomLayout", size_of::<SelectCustomLayoutRequest>()),
            ("SelectLayout", size_of::<SelectLayoutRequest>()),
            ("SelectPane", size_of::<SelectPaneRequest>()),
            ("SelectPaneAdjacent", size_of::<SelectPaneAdjacentRequest>()),
            ("SelectPaneMark", size_of::<SelectPaneMarkRequest>()),
            ("SetEnvironment", size_of::<SetEnvironmentRequest>()),
            ("SetBuffer", size_of::<SetBufferRequest>()),
            ("SetHook", size_of::<SetHookRequest>()),
            ("SetHookMutation", size_of::<SetHookMutationRequest>()),
            ("SetOption", size_of::<SetOptionRequest>()),
            ("ShowBuffer", size_of::<ShowBufferRequest>()),
            ("ShowEnvironment", size_of::<ShowEnvironmentRequest>()),
            ("ShowHooks", size_of::<ShowHooksRequest>()),
            ("ShowMessages", size_of::<ShowMessagesRequest>()),
            ("ShowOptions", size_of::<ShowOptionsRequest>()),
            ("SplitWindow", size_of::<SplitWindowRequest>()),
            ("SplitWindowExt", size_of::<SplitWindowExtRequest>()),
            (
                "SubscribePaneOutput",
                size_of::<SubscribePaneOutputRequest>(),
            ),
            (
                "SubscribePaneOutputRef",
                size_of::<SubscribePaneOutputRefRequest>(),
            ),
            ("SwitchClientExt2", size_of::<SwitchClientExt2Request>()),
        ];
        remaining.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
        eprintln!("PR6G largest remaining candidates: {remaining:?}");

        assert!(
            request_size <= 96,
            "Request grew past the PR6G boxed payload budget: {request_size}"
        );
        assert_eq!(box_size, size_of::<usize>());
    }

    #[test]
    fn pr6g_boxed_payloads_are_bincode_transparent_for_request_candidates() {
        assert_box_serializes_like_value(NewSessionExtRequest {
            session_name: Some(alpha()),
            working_directory: Some("/tmp".to_owned()),
            detached: true,
            size: Some(crate::TerminalSize {
                cols: 120,
                rows: 40,
            }),
            environment: Some(vec!["A=B".to_owned()]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: Some(vec!["read-only".to_owned()]),
            window_name: Some("main".to_owned()),
            print_session_info: true,
            print_format: Some("#{session_name}".to_owned()),
            command: Some(vec!["sh".to_owned(), "-lc".to_owned(), "true".to_owned()]),
            process_command: None,
            client_environment: Some(vec!["PATH=/bin".to_owned()]),
            skip_environment_update: false,
        });
        assert_box_serializes_like_value(SetOptionByNameRequest {
            scope: OptionScopeSelector::ServerGlobal,
            name: "@plugin".to_owned(),
            value: Some("enabled".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: true,
            format_target: Some(crate::Target::Session(alpha())),
        });
        assert_box_serializes_like_value(ListWindowsRequest {
            target: alpha(),
            format: Some("#{window_index}".to_owned()),
            filter: Some("#{==:#{window_active},1}".to_owned()),
            sort_order: Some("index".to_owned()),
            reversed: true,
        });
        assert_box_serializes_like_value(ListPanesRequest {
            target: alpha(),
            target_window_index: Some(1),
            format: Some("#{pane_index}".to_owned()),
            filter: Some("#{==:#{pane_active},1}".to_owned()),
            sort_order: Some("index".to_owned()),
            reversed: true,
        });
        assert_box_serializes_like_value(DisplayPanesRequest {
            target: alpha(),
            duration_ms: Some(1_000),
            non_blocking: true,
            no_command: false,
            template: Some("select-pane -t '%%'".to_owned()),
            target_client: Some("/dev/pts/1".to_owned()),
        });
        assert_box_serializes_like_value(SetBufferRequest {
            name: Some("named".to_owned()),
            content: b"buffer".to_vec(),
            append: false,
            new_name: None,
            set_clipboard: true,
            target_client: Some("/dev/pts/1".to_owned()),
        });
        assert_box_serializes_like_value(LoadBufferRequest {
            path: "/tmp/input".to_owned(),
            cwd: Some(PathBuf::from("/tmp")),
            name: Some("loaded".to_owned()),
            set_clipboard: true,
            target_client: Some("/dev/pts/1".to_owned()),
        });
        assert_box_serializes_like_value(AttachSessionExt3Request::from_ext2(
            AttachSessionExt2Request {
                target: Some(alpha()),
                target_spec: Some("alpha:0.0".to_owned()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: true,
                skip_environment_update: false,
                flags: Some(vec!["active-pane".to_owned()]),
                working_directory: Some("/tmp".to_owned()),
                client_terminal: crate::ClientTerminalContext::default(),
                client_size: Some(crate::TerminalSize { cols: 80, rows: 24 }),
            },
            vec![crate::CAPABILITY_ATTACH_RENDER.to_owned()],
        ));
        assert_box_serializes_like_value(WebShareRequest::Create(CreateWebShareRequest {
            scope: WebShareScope::Session(alpha()),
            public_base_url: Some("https://example.invalid".to_owned()),
            tunnel_provider: None,
            frontend_url: Some("https://share.example.invalid".to_owned()),
            ttl_seconds: Some(60),
            expires_at_unix: None,
            max_spectators: Some(8),
            max_operators: Some(1),
            url_options: WebShareUrlOptions::default(),
            require_pin: true,
            operator_pin: Some("123456".to_owned()),
            spectator_pin: Some("654321".to_owned()),
            terminal_palette: None,
            operator: true,
            spectator: true,
            controls: true,
            kill_session_on_expire: false,
        }));
    }

    fn assert_box_serializes_like_value<T>(value: T)
    where
        T: Clone + Serialize,
    {
        let boxed = Box::new(value.clone());
        assert_eq!(
            bincode::serialize(&value).expect("value encodes"),
            bincode::serialize(&boxed).expect("boxed value encodes"),
            "Box<T> must be transparent before PR6G boxes enum variants"
        );
    }
}
