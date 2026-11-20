//! Routing names and printable-output access for `Response` variants.

use super::{CommandOutput, Response};

impl Response {
    /// Returns the stable routing name for the response variant.
    ///
    /// `Error` is not tied to one command on the current wire and therefore
    /// reports the generic `error` tag.
    #[must_use]
    pub const fn command_name(&self) -> &'static str {
        match self {
            Self::SourceFileQueueV1(_) => "source-file-queue-v1",
            Self::RuntimeCommandExecution(_) => "runtime-command-execution",
            Self::SplitWindowCommandV1(_) => "split-window-command-v1",
            Self::NewPaneInputV1(_) => "new-pane-input-v1",
            Self::ExportWindowSnapshotV2(_) => "export-window-snapshot-v2",
            Self::RestoreWindowSnapshotV2(_) => "restore-window-snapshot-v2",
            Self::ExportWindowSnapshot(_) => "export-window-snapshot",
            Self::RestoreWindowSnapshot(_) => "restore-window-snapshot",
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
            Self::SplitWindow(_) | Self::SplitWindowIdentity(_) => "split-window",
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
            Self::SubscribePaneStream(_) => "subscribe-pane-stream",
            Self::PaneStreamCursor(_) => "pane-stream-cursor",
            Self::UnsubscribePaneStream(_) => "unsubscribe-pane-stream",
            Self::SdkWaitForOutput(_) => "sdk-wait-output",
            Self::CancelSdkWait(_) => "cancel-sdk-wait",
            Self::PaneBroadcastInput(_) => "send-keys",
            Self::PaneOptionSet(_) => "pane-option-set",
            Self::PaneOptionGet(_) => "pane-option-get",
            Self::SubscribePaneState(_) => "subscribe-pane-state",
            Self::PaneStateCursor(_) => "pane-state-cursor",
            Self::PaneStateLag(_) => "pane-state-lag",
            Self::UnsubscribePaneState(_) => "unsubscribe-pane-state",
            Self::PaneForegroundState(_) => "pane-foreground-state",
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
            Self::SourceFileQueueV1(response) => response.source.command_output(),
            Self::RuntimeCommandExecution(_) => None,
            Self::SplitWindowCommandV1(_) => None,
            Self::NewPaneInputV1(_) => None,
            Self::ExportWindowSnapshotV2(response) => Some(response.command_output()),
            Self::RestoreWindowSnapshotV2(_) => None,
            Self::ExportWindowSnapshot(response) => Some(response.command_output()),
            Self::RestoreWindowSnapshot(_) => None,
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
            | Self::SubscribePaneStream(_)
            | Self::PaneStreamCursor(_)
            | Self::UnsubscribePaneStream(_)
            | Self::SdkWaitForOutput(_)
            | Self::CancelSdkWait(_)
            | Self::PaneBroadcastInput(_)
            | Self::PaneOptionSet(_)
            | Self::PaneOptionGet(_)
            | Self::SubscribePaneState(_)
            | Self::PaneStateCursor(_)
            | Self::PaneStateLag(_)
            | Self::UnsubscribePaneState(_)
            | Self::PaneForegroundState(_)
            | Self::SplitWindowIdentity(_)
            | Self::CreateSessionLease(_)
            | Self::RenewSessionLease(_)
            | Self::ReleaseSessionLease(_)
            | Self::DaemonStatus(_)
            | Self::ShutdownIfIdle(_) => None,
            _ => None,
        }
    }
}
