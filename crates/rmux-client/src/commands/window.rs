use rmux_proto::{
    KillWindowRequest, LastWindowRequest, LayoutName, LinkWindowRequest, ListWindowsRequest,
    MoveWindowRequest, MoveWindowTarget, NewWindowRequest, NextWindowRequest,
    PreviousWindowRequest, RenameWindowRequest, Request, Response, RotateWindowDirection,
    RotateWindowRequest, SelectCustomLayoutRequest, SelectLayoutRequest, SelectLayoutTarget,
    SelectOldLayoutRequest, SelectWindowRequest, SessionName, SplitDirection,
    SplitWindowExtRequest, SplitWindowRequest, SplitWindowTarget, SplitWindowTargetActionRequest,
    SpreadLayoutRequest, SwapWindowRequest, UnlinkWindowRequest, WindowTarget,
};

use crate::{connection::Connection, ClientError};

/// Full options for a `split-window` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitWindowOptions {
    /// The exact split target.
    pub target: SplitWindowTarget,
    /// Axis on which to split (`Vertical` = stacked, `Horizontal` = side-by-side).
    pub direction: SplitDirection,
    /// `true` to insert the new pane *before* the target on the chosen axis
    /// (tmux `-b`); `false` to insert after (default).
    pub before: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    pub environment: Option<Vec<String>>,
    /// Optional command argv for the new pane.
    pub command: Option<Vec<String>>,
}

impl Connection {
    /// Sends a `new-window` request over the detached RPC channel.
    pub fn new_window(
        &mut self,
        target: rmux_proto::SessionName,
        name: Option<String>,
        detached: bool,
    ) -> Result<Response, ClientError> {
        self.new_window_with_environment(target, name, detached, None, None, None)
    }

    /// Sends a `new-window` request with explicit spawn environment overrides.
    pub fn new_window_with_environment(
        &mut self,
        target: rmux_proto::SessionName,
        name: Option<String>,
        detached: bool,
        environment: Option<Vec<String>>,
        start_directory: Option<std::path::PathBuf>,
        command: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        self.new_window_at_with_environment(
            target,
            None,
            name,
            detached,
            environment,
            start_directory,
            command,
            false,
        )
    }

    /// Sends a `new-window` request with an optional destination index.
    #[allow(clippy::too_many_arguments)]
    pub fn new_window_at_with_environment(
        &mut self,
        target: rmux_proto::SessionName,
        target_window_index: Option<u32>,
        name: Option<String>,
        detached: bool,
        environment: Option<Vec<String>>,
        start_directory: Option<std::path::PathBuf>,
        command: Option<Vec<String>>,
        insert_at_target: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::NewWindow(Box::new(NewWindowRequest {
            target,
            name,
            detached,
            start_directory,
            environment,
            command,
            process_command: None,
            target_window_index,
            insert_at_target,
        })))
    }

    /// Sends a `kill-window` request over the detached RPC channel.
    pub fn kill_window(
        &mut self,
        target: WindowTarget,
        kill_others: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::KillWindow(KillWindowRequest {
            target,
            kill_all_others: kill_others,
        }))
    }

    /// Sends a `select-window` request over the detached RPC channel.
    pub fn select_window(&mut self, target: WindowTarget) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectWindow(SelectWindowRequest { target }))
    }

    /// Sends a `rename-window` request over the detached RPC channel.
    pub fn rename_window(
        &mut self,
        target: WindowTarget,
        new_name: String,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RenameWindow(RenameWindowRequest {
            target,
            name: new_name,
        }))
    }

    /// Sends a `next-window` request over the detached RPC channel.
    pub fn next_window(
        &mut self,
        target: SessionName,
        alerts_only: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::NextWindow(NextWindowRequest {
            target,
            alerts_only,
        }))
    }

    /// Sends a `previous-window` request over the detached RPC channel.
    pub fn previous_window(
        &mut self,
        target: SessionName,
        alerts_only: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::PreviousWindow(PreviousWindowRequest {
            target,
            alerts_only,
        }))
    }

    /// Sends a `last-window` request over the detached RPC channel.
    pub fn last_window(&mut self, target: SessionName) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LastWindow(LastWindowRequest { target }))
    }

    /// Sends a `list-windows` request over the detached RPC channel.
    pub fn list_windows(
        &mut self,
        target: SessionName,
        format: Option<String>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListWindows(ListWindowsRequest { target, format }))
    }

    /// Sends a `link-window` request over the detached RPC channel.
    pub fn link_window(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        after: bool,
        before: bool,
        kill_destination: bool,
        detached: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::LinkWindow(LinkWindowRequest {
            source,
            target,
            after,
            before,
            kill_destination,
            detached,
        }))
    }

    /// Sends a `move-window` request over the detached RPC channel.
    pub fn move_window(
        &mut self,
        source: Option<WindowTarget>,
        target: MoveWindowTarget,
        renumber: bool,
        kill_destination: bool,
        detached: bool,
    ) -> Result<Response, ClientError> {
        self.move_window_with_position(
            source,
            target,
            renumber,
            kill_destination,
            detached,
            false,
            false,
        )
    }

    /// Sends a `move-window` request with optional `-a`/`-b` placement over the detached RPC channel.
    #[allow(clippy::too_many_arguments)]
    pub fn move_window_with_position(
        &mut self,
        source: Option<WindowTarget>,
        target: MoveWindowTarget,
        renumber: bool,
        kill_destination: bool,
        detached: bool,
        after: bool,
        before: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::MoveWindow(MoveWindowRequest {
            source,
            target,
            renumber,
            kill_destination,
            detached,
            after,
            before,
        }))
    }

    /// Sends a `swap-window` request over the detached RPC channel.
    pub fn swap_window(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        detached: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SwapWindow(SwapWindowRequest {
            source,
            target,
            detached,
        }))
    }

    /// Sends a `rotate-window` request over the detached RPC channel.
    pub fn rotate_window(
        &mut self,
        target: WindowTarget,
        direction: RotateWindowDirection,
    ) -> Result<Response, ClientError> {
        self.rotate_window_with_zoom(target, direction, false)
    }

    /// Sends a `rotate-window` request with zoom save/restore over the detached RPC channel.
    pub fn rotate_window_with_zoom(
        &mut self,
        target: WindowTarget,
        direction: RotateWindowDirection,
        restore_zoom: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RotateWindow(RotateWindowRequest {
            target,
            direction,
            restore_zoom,
        }))
    }

    /// Sends a `resize-window` request over the detached RPC channel.
    pub fn resize_window(
        &mut self,
        target: WindowTarget,
        width: Option<u16>,
        height: Option<u16>,
        adjustment: Option<rmux_proto::ResizeWindowAdjustment>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ResizeWindow(rmux_proto::ResizeWindowRequest {
            target,
            width,
            height,
            adjustment,
        }))
    }

    /// Sends an `unlink-window` request over the detached RPC channel.
    pub fn unlink_window(
        &mut self,
        target: WindowTarget,
        kill_if_last: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::UnlinkWindow(UnlinkWindowRequest {
            target,
            kill_if_last,
        }))
    }

    /// Sends a `respawn-window` request over the detached RPC channel.
    pub fn respawn_window(
        &mut self,
        target: WindowTarget,
        kill: bool,
    ) -> Result<Response, ClientError> {
        self.respawn_window_with_environment(target, kill, None, None, None)
    }

    /// Sends a `respawn-window` request with explicit spawn environment overrides.
    pub fn respawn_window_with_environment(
        &mut self,
        target: WindowTarget,
        kill: bool,
        environment: Option<Vec<String>>,
        start_directory: Option<std::path::PathBuf>,
        command: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::RespawnWindow(Box::new(
            rmux_proto::RespawnWindowRequest {
                target,
                kill,
                start_directory,
                environment,
                command,
            },
        )))
    }

    /// Sends a `split-window` request over the detached RPC channel.
    pub fn split_window(&mut self, target: SplitWindowTarget) -> Result<Response, ClientError> {
        self.split_window_with_direction(target, SplitDirection::Vertical)
    }

    /// Sends a `split-window` request with an explicit direction over the detached RPC channel.
    pub fn split_window_with_direction(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
    ) -> Result<Response, ClientError> {
        self.split_window_with_direction_and_environment(target, direction, None)
    }

    /// Sends a `split-window` request with explicit spawn environment overrides.
    pub fn split_window_with_direction_and_environment(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
        environment: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        self.split_window_with_spawn(target, direction, environment, None)
    }

    /// Sends a `split-window` request with explicit spawn options.
    ///
    /// New pane is inserted *after* the target. To insert before (tmux `-b`),
    /// use [`Connection::split_window_with_options`].
    pub fn split_window_with_spawn(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
        environment: Option<Vec<String>>,
        command: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        self.split_window_with_start_directory(target, direction, environment, None, command)
    }

    /// Sends a `split-window` request with an optional working-directory override.
    ///
    /// New pane is inserted *after* the target. To insert before (tmux `-b`),
    /// use [`Connection::split_window_with_options`] without a start directory.
    pub fn split_window_with_start_directory(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
        environment: Option<Vec<String>>,
        start_directory: Option<std::path::PathBuf>,
        command: Option<Vec<String>>,
    ) -> Result<Response, ClientError> {
        if command.is_some() || start_directory.is_some() {
            return self.roundtrip(&Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                target,
                direction,
                before: false,
                environment,
                command,
                process_command: None,
                start_directory,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            })));
        }
        self.split_window_with_options(SplitWindowOptions {
            target,
            direction,
            before: false,
            environment,
            command,
        })
    }

    /// Sends a `split-window` request with full options including `before`.
    pub fn split_window_with_options(
        &mut self,
        options: SplitWindowOptions,
    ) -> Result<Response, ClientError> {
        let SplitWindowOptions {
            target,
            direction,
            before,
            environment,
            command,
        } = options;
        if command.is_some() {
            return self.roundtrip(&Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
                target,
                direction,
                before,
                environment,
                command,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            })));
        }
        self.roundtrip(&Request::SplitWindow(SplitWindowRequest {
            target,
            direction,
            before,
            environment,
        }))
    }

    /// Sends a `split-window` request with server-side target resolution.
    pub fn split_window_target_action(
        &mut self,
        request: SplitWindowTargetActionRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SplitWindowTargetAction(Box::new(request)))
    }

    /// Sends a `select-layout` request over the detached RPC channel.
    pub fn select_layout(
        &mut self,
        target: SelectLayoutTarget,
        layout: LayoutName,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectLayout(SelectLayoutRequest {
            target,
            layout,
        }))
    }

    /// Sends a `select-layout` custom layout request over the detached RPC channel.
    pub fn select_custom_layout(
        &mut self,
        target: SelectLayoutTarget,
        layout: String,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target,
            layout,
        }))
    }

    /// Sends a `select-layout -o` request over the detached RPC channel.
    pub fn select_old_layout(
        &mut self,
        target: SelectLayoutTarget,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SelectOldLayout(SelectOldLayoutRequest { target }))
    }

    /// Sends a `select-layout -E` request over the detached RPC channel.
    pub fn spread_layout(&mut self, target: SelectLayoutTarget) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SpreadLayout(SpreadLayoutRequest { target }))
    }

    /// Sends a `next-layout` request over the detached RPC channel.
    pub fn next_layout(&mut self, target: WindowTarget) -> Result<Response, ClientError> {
        self.roundtrip(&Request::NextLayout(rmux_proto::NextLayoutRequest {
            target,
        }))
    }

    /// Sends a `previous-layout` request over the detached RPC channel.
    pub fn previous_layout(&mut self, target: WindowTarget) -> Result<Response, ClientError> {
        self.roundtrip(&Request::PreviousLayout(
            rmux_proto::PreviousLayoutRequest { target },
        ))
    }
}
