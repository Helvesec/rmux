use rmux_proto::{
    CapturePaneRequest, ErrorResponse, ResizePaneRequest, Response, RmuxError, SplitWindowTarget,
    Target,
};

use super::{pane_support::SplitWindowParts, RequestHandler};

impl RequestHandler {
    pub(in crate::handler) async fn handle_split_window_target_action(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowTargetActionRequest,
    ) -> Response {
        let target = match self
            .resolve_target_for_requester(
                requester_pid,
                rmux_proto::ResolveTargetRequest {
                    target: request.target,
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                },
            )
            .await
        {
            Ok(Target::Pane(target)) => SplitWindowTarget::Pane(target),
            Ok(_) => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget {
                        value: "split-window".to_owned(),
                        reason: "resolved target is not a pane".to_owned(),
                    },
                })
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target,
                direction: request.direction,
                before: request.before,
                environment_overrides: request.environment,
                command: request.command,
                process_command: request.process_command,
                start_directory: request.start_directory,
                keep_alive_on_exit: request.keep_alive_on_exit,
                detached: request.detached,
                size: request.size,
                preserve_zoom: request.preserve_zoom,
                full_size: request.full_size,
                stdin_payload: request.stdin_payload,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_resize_pane_target_action(
        &self,
        requester_pid: u32,
        request: rmux_proto::ResizePaneTargetActionRequest,
    ) -> Response {
        let target = match self
            .resolve_target_for_requester(
                requester_pid,
                rmux_proto::ResolveTargetRequest {
                    target: request.target,
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                },
            )
            .await
        {
            Ok(Target::Pane(target)) => target,
            Ok(_) => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget {
                        value: "resize-pane".to_owned(),
                        reason: "resolved target is not a pane".to_owned(),
                    },
                })
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        self.handle_resize_pane(ResizePaneRequest {
            target,
            adjustment: request.adjustment,
        })
        .await
    }

    pub(in crate::handler) async fn handle_capture_pane_target_action(
        &self,
        requester_pid: u32,
        request: rmux_proto::CapturePaneTargetActionRequest,
    ) -> Response {
        let target = match self
            .resolve_target_for_requester(
                requester_pid,
                rmux_proto::ResolveTargetRequest {
                    target: request.target,
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                },
            )
            .await
        {
            Ok(Target::Pane(target)) => target,
            Ok(_) => {
                return Response::Error(ErrorResponse {
                    error: RmuxError::InvalidTarget {
                        value: "capture-pane".to_owned(),
                        reason: "resolved target is not a pane".to_owned(),
                    },
                })
            }
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        self.handle_capture_pane(CapturePaneRequest {
            target,
            start: request.start,
            end: request.end,
            print: request.print,
            buffer_name: request.buffer_name,
            alternate: request.alternate,
            escape_ansi: request.escape_ansi,
            escape_sequences: request.escape_sequences,
            join_wrapped: request.join_wrapped,
            use_mode_screen: request.use_mode_screen,
            preserve_trailing_spaces: request.preserve_trailing_spaces,
            do_not_trim_spaces: request.do_not_trim_spaces,
            pending_input: request.pending_input,
            quiet: request.quiet,
            start_is_absolute: request.start_is_absolute,
            end_is_absolute: request.end_is_absolute,
        })
        .await
    }
}
