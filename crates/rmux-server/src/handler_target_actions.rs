use rmux_proto::{
    CapturePaneRequest, ErrorResponse, PaneId, ResizePaneRequest, Response, RmuxError,
    SplitWindowTarget, Target,
};

use super::{
    pane_support::{SplitWindowParts, SplitWindowResponseMode},
    RequestHandler,
};

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct SplitTargetResolutionPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static SPLIT_TARGET_RESOLUTION_PAUSE: std::sync::Mutex<
    Option<(
        rmux_proto::SessionName,
        PaneId,
        std::sync::Arc<SplitTargetResolutionPause>,
    )>,
> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(in crate::handler) fn install_split_target_resolution_pause(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
) -> std::sync::Arc<SplitTargetResolutionPause> {
    let pause = std::sync::Arc::new(SplitTargetResolutionPause::default());
    *SPLIT_TARGET_RESOLUTION_PAUSE
        .lock()
        .expect("split target resolution pause lock") =
        Some((session_name, pane_id, pause.clone()));
    pause
}

#[cfg(test)]
async fn pause_after_split_target_resolution(
    target: &SplitWindowTarget,
    expected_pane_id: Option<PaneId>,
) {
    let target_session = match target {
        SplitWindowTarget::Session(session_name) => session_name,
        SplitWindowTarget::Pane(target) => target.session_name(),
    };
    let pause = {
        let mut installed = SPLIT_TARGET_RESOLUTION_PAUSE
            .lock()
            .expect("split target resolution pause lock");
        let matches_pane = installed
            .as_ref()
            .is_some_and(|(session_name, pane_id, _)| {
                session_name == target_session && Some(*pane_id) == expected_pane_id
            });
        matches_pane.then(|| {
            installed
                .take()
                .expect("matching split target resolution pause remains installed")
                .2
        })
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(not(test))]
async fn pause_after_split_target_resolution(
    _target: &SplitWindowTarget,
    _expected_pane_id: Option<PaneId>,
) {
}

fn stable_pane_id_target(target: Option<&str>) -> Option<PaneId> {
    target?
        .strip_prefix('%')?
        .parse::<u32>()
        .ok()
        .map(PaneId::new)
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_split_window_target_action(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowTargetActionRequest,
    ) -> Response {
        self.handle_split_window_target_action_with_mode(
            requester_pid,
            request,
            SplitWindowResponseMode::Legacy,
        )
        .await
    }

    pub(in crate::handler) async fn handle_split_window_identity(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowIdentityRequest,
    ) -> Response {
        self.handle_split_window_target_action_with_mode(
            requester_pid,
            request.action,
            SplitWindowResponseMode::StableIdentity,
        )
        .await
    }

    async fn handle_split_window_target_action_with_mode(
        &self,
        requester_pid: u32,
        request: rmux_proto::SplitWindowTargetActionRequest,
        response_mode: SplitWindowResponseMode,
    ) -> Response {
        let expected_pane_id = stable_pane_id_target(request.target.as_deref());
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
        pause_after_split_target_resolution(&target, expected_pane_id).await;

        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target,
                expected_pane_id,
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
                response_mode,
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
            include_format: request.include_format,
            hyperlinks: request.hyperlinks,
            line_numbers: request.line_numbers,
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
