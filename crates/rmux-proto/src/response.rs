//! Detached response contracts.

use serde::{Deserialize, Serialize};

use crate::{ControlModeResponse, HandshakeResponse, LayoutName, RmuxError, SdkWaitId};

mod routing;

mod session;
pub use session::{
    CreateSessionLeaseResponse, HasSessionResponse, KillSessionResponse, ListSessionsResponse,
    NewSessionResponse, ReleaseSessionLeaseResponse, RenameSessionResponse,
    RenewSessionLeaseResponse,
};

mod server;
pub use server::{
    DaemonStatusResponse, KillServerResponse, LockClientResponse, LockServerResponse,
    LockSessionResponse, ServerAccessResponse, ShutdownIfIdleResponse,
};

mod target;
pub use target::ResolveTargetResponse;

mod window;
pub use window::{
    KillWindowResponse, LastWindowResponse, LinkWindowResponse, ListWindowsResponse,
    MoveWindowResponse, NewWindowResponse, NextWindowResponse, PreviousWindowResponse,
    RenameWindowResponse, ResizeWindowResponse, RespawnWindowResponse, RotateWindowResponse,
    SelectWindowResponse, SwapWindowResponse, UnlinkWindowResponse, WindowListEntry,
};

mod pane;
pub use pane::{
    BreakPaneResponse, DisplayPanesResponse, ForegroundFieldSource, ForegroundSourcesDto,
    ForegroundStateDto, JoinPaneResponse, KillPaneResponse, LastPaneResponse, ListPanesResponse,
    MovePaneResponse, PaneBroadcastInputFailure, PaneBroadcastInputResponse,
    PaneBroadcastInputSuccess, PaneForegroundStateResponse, PaneOptionEntry, PaneOptionGetResponse,
    PaneOptionSetResponse, PaneOutputCursor, PaneOutputCursorResponse, PaneOutputEvent,
    PaneOutputLagNotice, PaneOutputLagResponse, PaneRawBytes, PaneRawRebase, PaneRawRebaseReason,
    PaneRecentOutput, PaneRecoveryCoverage, PaneSnapshotCell, PaneSnapshotCursor,
    PaneSnapshotResponse, PaneStateClosedReason, PaneStateCursorResponse, PaneStateEventDto,
    PaneStateLagResponse, PaneStateSnapshot, PaneStreamCursorResponse, PaneStreamEndReason,
    PaneStreamEvent, PaneStreamLifecycleEvent, PaneSurfaceDynamicColors, PaneSurfaceFrame,
    PaneSurfaceHyperlink, PaneSurfaceSnapshot, PipePaneResponse, ResizePaneResponse,
    RespawnPaneResponse, SelectPaneResponse, SendKeysResponse, SplitWindowIdentityResponse,
    SplitWindowResponse, SubscribePaneOutputResponse, SubscribePaneStateResponse,
    SubscribePaneStreamResponse, SwapPaneResponse, UnsubscribePaneOutputResponse,
    UnsubscribePaneStateResponse, UnsubscribePaneStreamResponse,
};

mod client;
pub use client::{
    AttachSessionResponse, DetachClientResponse, ListClientsResponse, RefreshClientResponse,
    SuspendClientResponse, SwitchClientResponse,
};

mod keys;
pub use keys::{
    BindKeyResponse, ClockModeResponse, CopyModeResponse, ListKeysResponse, SendPrefixResponse,
    UnbindKeyResponse,
};

mod options;
pub use options::{
    SetEnvironmentResponse, SetHookResponse, SetOptionByNameResponse, SetOptionResponse,
    ShowEnvironmentResponse, ShowHooksResponse, ShowOptionsResponse,
};

mod web;
pub use web::{
    WebShareConfigResponse, WebShareCreatedResponse, WebShareListResponse, WebShareListener,
    WebShareLookupResponse, WebShareResponse, WebShareStoppedAllResponse, WebShareStoppedResponse,
    WebShareSummary,
};

mod buffer;
pub use buffer::{
    DeleteBufferResponse, ListBuffersResponse, LoadBufferResponse, PasteBufferResponse,
    SaveBufferResponse, SetBufferResponse, ShowBufferResponse,
};

mod shell;
pub use shell::{IfShellResponse, RunShellResponse, SourceFileResponse};

mod snapshot;
pub use snapshot::{
    ExportWindowSnapshotResponse, ExportWindowSnapshotV2Response, RestoreWindowSnapshotResponse,
    RestoreWindowSnapshotV2Response,
};

/// All detached responses supported by the wire protocol.
#[non_exhaustive]
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
    /// Success payload for SDK pane option mutation.
    PaneOptionSet(Box<PaneOptionSetResponse>),
    /// Success payload for SDK pane option lookup.
    PaneOptionGet(PaneOptionGetResponse),
    /// Success payload for SDK pane-state subscription.
    SubscribePaneState(Box<SubscribePaneStateResponse>),
    /// Success payload for SDK pane-state cursor.
    PaneStateCursor(PaneStateCursorResponse),
    /// Lag notice for SDK pane-state cursor.
    PaneStateLag(Box<PaneStateLagResponse>),
    /// Success payload for SDK pane-state unsubscription.
    UnsubscribePaneState(UnsubscribePaneStateResponse),
    /// Success payload for SDK pane foreground-state lookup.
    PaneForegroundState(Box<PaneForegroundStateResponse>),
    /// Atomic visible and stable identity returned by an SDK split.
    SplitWindowIdentity(SplitWindowIdentityResponse),
    /// Atomic initial recoverable pane stream event.
    SubscribePaneStream(Box<SubscribePaneStreamResponse>),
    /// Batched recoverable pane stream events.
    PaneStreamCursor(Box<PaneStreamCursorResponse>),
    /// Recoverable pane stream unsubscription result.
    UnsubscribePaneStream(UnsubscribePaneStreamResponse),
    /// Negotiation or completion result of the CLI argv execution offer.
    RuntimeCommandExecution(Box<crate::RuntimeCommandExecutionResponse>),
    /// Terminal result of explicit split/key command admission.
    SplitWindowCommandV1(Box<crate::SplitWindowCommandV1Response>),
    /// Success payload for `export-window-snapshot`.
    ExportWindowSnapshot(Box<ExportWindowSnapshotResponse>),
    /// Success payload for `restore-window-snapshot`.
    RestoreWindowSnapshot(RestoreWindowSnapshotResponse),
    /// Terminal result of the `new-pane -I` offer.
    NewPaneInputV1(Box<crate::NewPaneInputV1Response>),
    /// Explicit V2 canonical snapshot response.
    ExportWindowSnapshotV2(Box<ExportWindowSnapshotV2Response>),
    /// Explicit V2 canonical restore response.
    RestoreWindowSnapshotV2(RestoreWindowSnapshotV2Response),
    /// Source completion with explicitly negotiated CLI queue disposition.
    SourceFileQueueV1(Box<crate::SourceFileQueueV1Response>),
    /// The running source has reached a consumer. Send one SourceFileInputDataV1.
    SourceFileReadStdinV1(crate::SourceFileReadStdinV1Response),
    /// Transfer the stdin offer to an independent reader before completion.
    SourceFileRelayV1(crate::SourceFileRelayV1Response),
    /// Confirm the claim and prove ownership of the offer.
    SourceFileRelayReadyV1(crate::SourceFileRelayReadyV1Response),
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

/// Response payload for `wait-for`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForResponse;

/// Terminal state for a daemon-backed SDK byte wait.
#[non_exhaustive]
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
    /// Armed acknowledgement is deliberately kept outside the terminal
    /// `SdkWaitOutcome` enum. This preserves the current release wire while the
    /// non-exhaustive outcome type remains forward-compatible for SDK callers.
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
    fn run_shell_response_rejects_old_unversioned_payload() {
        let bytes = bincode::serialize(&OldRunShellResponse { output: None })
            .expect("old run-shell response serializes");

        bincode::deserialize::<RunShellResponse>(&bytes)
            .expect_err("current response rejects old payload without a matching wire version");
    }

    #[test]
    fn source_file_response_rejects_old_unversioned_payload() {
        let bytes = bincode::serialize(&OldSourceFileResponse {
            output: Some(CommandOutput::from_stdout(b"parsed\n".to_vec())),
            exit_status: Some(1),
        })
        .expect("old source-file response serializes");

        bincode::deserialize::<SourceFileResponse>(&bytes)
            .expect_err("current response rejects old payload without a matching wire version");
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
    fn known_sdk_wait_outcomes_are_terminal() {
        #[allow(unreachable_patterns)]
        fn is_terminal(outcome: SdkWaitOutcome) -> bool {
            matches!(outcome, SdkWaitOutcome::Matched | SdkWaitOutcome::Cancelled)
        }

        assert!(is_terminal(SdkWaitOutcome::Matched));
        assert!(is_terminal(SdkWaitOutcome::Cancelled));
    }

    #[test]
    fn pr6g_response_boxing_keeps_response_size_bounded() {
        assert_eq!(size_of::<Box<crate::RuntimeCommandExecutionResponse>>(), 8);
        assert!(size_of::<crate::RuntimeCommandExecutionResponse>() > 72);
        assert_eq!(size_of::<Box<WebShareResponse>>(), 8);
        assert_eq!(size_of::<Box<PaneOutputLagResponse>>(), 8);
        assert_eq!(size_of::<Box<PaneOptionSetResponse>>(), 8);
        assert_eq!(size_of::<Box<SubscribePaneStateResponse>>(), 8);
        assert_eq!(size_of::<Box<PaneForegroundStateResponse>>(), 8);
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
        assert!(
            size_of::<PaneOptionSetResponse>() > size_of::<Response>(),
            "PaneOptionSetResponse must remain boxed while it is larger than Response"
        );
        assert!(
            size_of::<SubscribePaneStateResponse>() > size_of::<Response>(),
            "SubscribePaneStateResponse must remain boxed while it is larger than Response"
        );
        assert!(
            size_of::<PaneForegroundStateResponse>() > size_of::<Response>(),
            "PaneForegroundStateResponse must remain boxed while it is larger than Response"
        );
    }

    #[test]
    fn runtime_command_execution_boxing_preserves_wire_bytes() {
        use crate::RuntimeCommandExecutionResponse as Execution;

        for payload in [
            Execution::Fallback,
            Execution::Unsupported(RmuxError::UnsupportedCapability {
                feature: "future.execution".to_owned(),
                supported: vec!["cli.runtime-command-execution".to_owned()],
            }),
            Execution::Complete {
                command_name: "display-message".to_owned(),
                stdout: vec![0, 255, b'\n'],
                error: None,
                exit_status: 0,
            },
            Execution::Complete {
                command_name: "run-shell".to_owned(),
                stdout: b"partial\n".to_vec(),
                error: Some(RmuxError::Decode("command error".to_owned())),
                exit_status: 17,
            },
        ] {
            // The published outer tag and unboxed payload define the old bytes.
            let mut expected = 105_u32.to_le_bytes().to_vec();
            expected.extend(bincode::serialize(&payload).unwrap());
            let response = Response::RuntimeCommandExecution(Box::new(payload));
            assert_eq!(bincode::serialize(&response).unwrap(), expected);
            assert_eq!(
                bincode::deserialize::<Response>(&expected).unwrap(),
                response
            );
        }
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
