use rmux_proto::{
    BreakPaneRequest, CancelSdkWaitRequest, ClockModeRequest, CopyModeRequest, DisplayPanesRequest,
    JoinPaneRequest, KillPaneRequest, LastPaneRequest, MovePaneRequest, PaneBroadcastInputRequest,
    PaneInputRequest, PaneOutputCursorRequest, PaneOutputSubscriptionId,
    PaneOutputSubscriptionStart, PaneSnapshotRefRequest, PaneTarget, PaneTargetRef,
    PipePaneRequest, Request, ResizePaneAdjustment, ResizePaneRequest,
    ResizePaneTargetActionRequest, RespawnPaneRequest, Response, SdkWaitForOutputRefRequest,
    SdkWaitId, SdkWaitOwnerId, SelectPaneAdjacentRequest, SelectPaneDirection,
    SelectPaneMarkRequest, SelectPaneRequest, SendKeysExt2Request, SendKeysExtRequest,
    SendKeysRequest, SendPrefixRequest, SessionName, SubscribePaneOutputRefRequest,
    SwapPaneDirection, SwapPaneRequest, UnsubscribePaneOutputRequest, WindowTarget,
    CAPABILITY_SDK_PANE_BROADCAST, CAPABILITY_SDK_PANE_BY_ID, CAPABILITY_SDK_WAITS,
    CAPABILITY_TARGET_CLIENT_COMMANDS,
};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `swap-pane` request over the detached RPC channel.
    pub fn swap_pane(
        &mut self,
        source: PaneTarget,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwapPane(SwapPaneRequest {
            source,
            target,
            direction: None,
            detached,
            preserve_zoom,
        }))
    }

    /// Sends `swap-pane -D` over the detached RPC channel.
    pub fn swap_pane_with_next(
        &mut self,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwapPane(SwapPaneRequest {
            source: target.clone(),
            target,
            direction: Some(SwapPaneDirection::Down),
            detached,
            preserve_zoom,
        }))
    }

    /// Sends `swap-pane -U` over the detached RPC channel.
    pub fn swap_pane_with_previous(
        &mut self,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwapPane(SwapPaneRequest {
            source: target.clone(),
            target,
            direction: Some(SwapPaneDirection::Up),
            detached,
            preserve_zoom,
        }))
    }

    /// Sends a `last-pane` request over the detached RPC channel.
    pub fn last_pane(&mut self, target: WindowTarget) -> Result<Response, ClientError> {
        self.last_pane_with_zoom(target, false)
    }

    /// Sends a `last-pane` request with optional zoom preservation.
    pub fn last_pane_with_zoom(
        &mut self,
        target: WindowTarget,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.last_pane_with_options(target, preserve_zoom, None)
    }

    /// Sends a `last-pane` request with tmux selection options.
    pub fn last_pane_with_options(
        &mut self,
        target: WindowTarget,
        preserve_zoom: bool,
        input_disabled: Option<bool>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LastPane(LastPaneRequest {
            target,
            preserve_zoom,
            input_disabled,
        }))
    }

    /// Sends a `join-pane` request over the detached RPC channel.
    pub fn join_pane(&mut self, request: JoinPaneRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::JoinPane(request))
    }

    /// Sends a `move-pane` request over the detached RPC channel.
    pub fn move_pane(&mut self, request: MovePaneRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::MovePane(request))
    }

    /// Sends a `break-pane` request over the detached RPC channel.
    pub fn break_pane(&mut self, request: BreakPaneRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::BreakPane(Box::new(request)))
    }

    /// Sends a `resize-pane` request over the detached RPC channel.
    pub fn resize_pane(
        &mut self,
        target: PaneTarget,
        adjustment: ResizePaneAdjustment,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ResizePane(ResizePaneRequest {
            target,
            adjustment,
        }))
    }

    /// Sends a `resize-pane` request with server-side target resolution.
    pub fn resize_pane_target_action(
        &mut self,
        request: ResizePaneTargetActionRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ResizePaneTargetAction(request))
    }

    /// Sends a `display-panes` request over the detached RPC channel.
    pub fn display_panes(
        &mut self,
        target: SessionName,
        duration_ms: Option<u64>,
        non_blocking: bool,
        no_command: bool,
        template: Option<String>,
    ) -> Result<Response, ClientError> {
        let request = Request::DisplayPanes(DisplayPanesRequest {
            target,
            duration_ms,
            non_blocking,
            no_command,
            template,
        });
        if non_blocking {
            self.roundtrip(&request)
        } else {
            self.roundtrip_without_read_timeout(&request)
        }
    }

    /// Sends a `pipe-pane` request over the detached RPC channel.
    pub fn pipe_pane(
        &mut self,
        target: PaneTarget,
        stdin: bool,
        stdout: bool,
        once: bool,
        command: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::PipePane(PipePaneRequest {
            target,
            stdin,
            stdout,
            once,
            command,
        }))
    }

    /// Sends a `respawn-pane` request over the detached RPC channel.
    pub fn respawn_pane(&mut self, request: RespawnPaneRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RespawnPane(Box::new(request)))
    }

    /// Sends a `select-pane` request over the detached RPC channel.
    pub fn select_pane(&mut self, target: PaneTarget) -> Result<Response, ClientError> {
        self.select_pane_with_title(target, None)
    }

    /// Sends a `select-pane` request with an optional title over the detached RPC channel.
    pub fn select_pane_with_title(
        &mut self,
        target: PaneTarget,
        title: Option<String>,
    ) -> Result<Response, ClientError> {
        self.select_pane_with_title_and_zoom(target, title, false)
    }

    /// Sends a `select-pane` request with optional title and zoom preservation.
    pub fn select_pane_with_title_and_zoom(
        &mut self,
        target: PaneTarget,
        title: Option<String>,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.select_pane_with_options(target, title, None, None, preserve_zoom)
    }

    /// Sends a `select-pane` request with optional title, style, input state, and zoom preservation.
    pub fn select_pane_with_options(
        &mut self,
        target: PaneTarget,
        title: Option<String>,
        style: Option<String>,
        input_disabled: Option<bool>,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title,
            input_disabled,
            preserve_zoom,
            style,
        })))
    }

    /// Sends a `select-pane -d` / `-e` input-state request.
    pub fn select_pane_input(
        &mut self,
        target: PaneTarget,
        disabled: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title: None,
            style: None,
            input_disabled: Some(disabled),
            preserve_zoom: false,
        })))
    }

    /// Sends a directional `select-pane` request over the detached RPC channel.
    pub fn select_pane_adjacent(
        &mut self,
        target: PaneTarget,
        direction: SelectPaneDirection,
    ) -> Result<Response, ClientError> {
        self.select_pane_adjacent_with_zoom(target, direction, false)
    }

    /// Sends a directional `select-pane` request with optional zoom preservation.
    pub fn select_pane_adjacent_with_zoom(
        &mut self,
        target: PaneTarget,
        direction: SelectPaneDirection,
        preserve_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target,
            direction,
            preserve_zoom,
        }))
    }

    /// Sends `select-pane -m` or `select-pane -M` over the detached RPC channel.
    pub fn select_pane_mark(
        &mut self,
        target: PaneTarget,
        clear: bool,
    ) -> Result<Response, ClientError> {
        self.select_pane_mark_with_title(target, clear, None)
    }

    /// Sends `select-pane -m/-M` with an optional title over the detached RPC channel.
    pub fn select_pane_mark_with_title(
        &mut self,
        target: PaneTarget,
        clear: bool,
        title: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectPaneMark(SelectPaneMarkRequest {
            target,
            clear,
            title,
        }))
    }

    /// Sends a `kill-pane` request over the detached RPC channel.
    pub fn kill_pane(&mut self, target: PaneTarget) -> Result<Response, ClientError> {
        self.kill_pane_with_options(target, false)
    }

    /// Sends a `kill-pane` request with extended tmux flags.
    pub fn kill_pane_with_options(
        &mut self,
        target: PaneTarget,
        kill_all_except: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::KillPane(KillPaneRequest {
            target,
            kill_all_except,
        }))
    }

    /// Sends a `send-keys` request over the detached RPC channel.
    pub fn send_keys(
        &mut self,
        target: PaneTarget,
        keys: Vec<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SendKeys(SendKeysRequest { target, keys }))
    }

    /// Sends an extended `send-keys` request over the detached RPC channel.
    pub fn send_keys_extended(
        &mut self,
        request: SendKeysExtRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SendKeysExt(request))
    }

    /// Sends a target-client aware `send-keys` request over detached RPC.
    pub fn send_keys_extended_target_client(
        &mut self,
        request: SendKeysExt2Request,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_TARGET_CLIENT_COMMANDS)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_TARGET_CLIENT_COMMANDS.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::SendKeysExt2(Box::new(request)))
    }

    /// Sends a stable-target SDK pane input request over detached RPC.
    pub fn pane_input(&mut self, request: PaneInputRequest) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_PANE_BY_ID)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_PANE_BY_ID.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::PaneInput(request))
    }

    /// Sends a stable-target SDK pane input broadcast request over detached RPC.
    pub fn pane_broadcast_input(
        &mut self,
        request: PaneBroadcastInputRequest,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_PANE_BROADCAST)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_PANE_BROADCAST.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::PaneBroadcastInput(request))
    }

    /// Captures a stable-target structured pane snapshot over detached RPC.
    pub fn pane_snapshot_ref(&mut self, target: PaneTargetRef) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_PANE_BY_ID)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_PANE_BY_ID.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::PaneSnapshotRef(PaneSnapshotRefRequest { target }))
    }

    /// Subscribes to stable-target pane output.
    pub fn subscribe_pane_output_ref(
        &mut self,
        target: PaneTargetRef,
        start: PaneOutputSubscriptionStart,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_PANE_BY_ID)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_PANE_BY_ID.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::SubscribePaneOutputRef(
            SubscribePaneOutputRefRequest { target, start },
        ))
    }

    /// Polls a pane-output subscription cursor.
    pub fn pane_output_cursor(
        &mut self,
        subscription_id: PaneOutputSubscriptionId,
        max_events: Option<u16>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::PaneOutputCursor(PaneOutputCursorRequest {
            subscription_id,
            max_events,
        }))
    }

    /// Unsubscribes from pane output.
    pub fn unsubscribe_pane_output(
        &mut self,
        subscription_id: PaneOutputSubscriptionId,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::UnsubscribePaneOutput(
            UnsubscribePaneOutputRequest { subscription_id },
        ))
    }

    /// Arms a daemon-backed stable-target SDK byte wait.
    pub fn sdk_wait_for_output_ref(
        &mut self,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
        target: PaneTargetRef,
        bytes: Vec<u8>,
        start: PaneOutputSubscriptionStart,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_WAITS)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_WAITS.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip_without_read_timeout(&Request::SdkWaitForOutputRef(
            SdkWaitForOutputRefRequest {
                owner_id,
                wait_id,
                target,
                bytes,
                start,
            },
        ))
    }

    /// Writes a stable-target SDK byte wait request without reading its response.
    ///
    /// Callers use this to arm a future-output wait before performing another
    /// action, then read the response later on the same connection.
    pub fn arm_sdk_wait_for_output_ref(
        &mut self,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
        target: PaneTargetRef,
        bytes: Vec<u8>,
        start: PaneOutputSubscriptionStart,
    ) -> Result<(), ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_WAITS)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_WAITS.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.write_request(&Request::SdkWaitForOutputRef(SdkWaitForOutputRefRequest {
            owner_id,
            wait_id,
            target,
            bytes,
            start,
        }))
    }

    /// Cancels a daemon-backed SDK byte wait.
    pub fn cancel_sdk_wait(
        &mut self,
        owner_id: SdkWaitOwnerId,
        wait_id: SdkWaitId,
    ) -> Result<Response, ClientError> {
        if !self.supports_capability(CAPABILITY_SDK_WAITS)? {
            return Err(ClientError::Protocol(
                rmux_proto::RmuxError::UnsupportedCapability {
                    feature: CAPABILITY_SDK_WAITS.to_owned(),
                    supported: Vec::new(),
                },
            ));
        }
        self.roundtrip(&Request::CancelSdkWait(CancelSdkWaitRequest {
            owner_id,
            wait_id,
        }))
    }

    /// Sends a `send-prefix` request over the detached RPC channel.
    pub fn send_prefix(
        &mut self,
        target: Option<PaneTarget>,
        secondary: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SendPrefix(SendPrefixRequest {
            target,
            secondary,
        }))
    }

    /// Sends a `copy-mode` request over the detached RPC channel.
    pub fn copy_mode(&mut self, request: CopyModeRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::CopyMode(request))
    }

    /// Sends a `clock-mode` request over the detached RPC channel.
    pub fn clock_mode(&mut self, target: Option<PaneTarget>) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ClockMode(ClockModeRequest { target }))
    }
}
