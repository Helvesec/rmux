use rmux_proto::{
    CapturePaneRequest, ErrorResponse, PaneId, ResizePaneRequest, Response, RmuxError,
    SplitWindowTarget, Target,
};

use super::{
    pane_support::{PaneStdinPolicy, SplitWindowParts, SplitWindowResponseMode},
    RequestHandler,
};

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct TargetResolutionPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static TARGET_RESOLUTION_PAUSE: std::sync::Mutex<
    Vec<(
        rmux_proto::SessionName,
        PaneId,
        std::sync::Arc<TargetResolutionPause>,
    )>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(in crate::handler) fn install_target_resolution_pause(
    session_name: rmux_proto::SessionName,
    pane_id: PaneId,
) -> std::sync::Arc<TargetResolutionPause> {
    let pause = std::sync::Arc::new(TargetResolutionPause::default());
    TARGET_RESOLUTION_PAUSE
        .lock()
        .expect("target resolution pause lock")
        .push((session_name, pane_id, pause.clone()));
    pause
}

#[cfg(test)]
async fn pause_after_target_resolution(
    target_session: &rmux_proto::SessionName,
    expected_pane_id: Option<PaneId>,
) {
    let pause = {
        let mut installed = TARGET_RESOLUTION_PAUSE
            .lock()
            .expect("split target resolution pause lock");
        let matching = installed.iter().position(|(session_name, pane_id, _)| {
            session_name == target_session && Some(*pane_id) == expected_pane_id
        });
        matching.map(|index| installed.swap_remove(index).2)
    };
    let Some(pause) = pause else {
        return;
    };
    pause.reached.notify_one();
    pause.release.notified().await;
}

#[cfg(not(test))]
async fn pause_after_target_resolution(
    _target_session: &rmux_proto::SessionName,
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
        let target_session = match &target {
            SplitWindowTarget::Session(session_name) => session_name,
            SplitWindowTarget::Pane(target) => target.session_name(),
        };
        pause_after_target_resolution(target_session, expected_pane_id).await;

        self.handle_split_window_parts(
            requester_pid,
            SplitWindowParts {
                target,
                expected_pane_id,
                floating: None,
                direction: request.direction,
                before: request.before,
                environment_overrides: request.environment,
                command: request.command,
                process_command: request.process_command,
                start_directory: request.start_directory,
                keep_alive_on_exit: request.keep_alive_on_exit.map(Into::into),
                detached: request.detached,
                size: request.size,
                preserve_zoom: request.preserve_zoom,
                full_size: request.full_size,
                stdin_payload: request.stdin_payload,
                stdin_policy: PaneStdinPolicy::default(),
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
        let captured = self
            .resolve_target_for_requester_with(
                requester_pid,
                rmux_proto::ResolveTargetRequest {
                    target: request.target,
                    target_type: rmux_proto::ResolveTargetType::Pane,
                    window_index: false,
                    prefer_unattached: false,
                },
                |state, target| {
                    let Target::Pane(target) = target else {
                        return Err(RmuxError::InvalidTarget {
                            value: "resize-pane".to_owned(),
                            reason: "resolved target is not a pane".to_owned(),
                        });
                    };
                    let pane_id = state
                        .sessions
                        .session(target.session_name())
                        .and_then(|session| session.window_at(target.window_index()))
                        .and_then(|window| window.pane(target.pane_index()))
                        .map(rmux_core::Pane::id)
                        .ok_or_else(|| {
                            RmuxError::invalid_target(
                                target.to_string(),
                                "resolved pane disappeared before resize",
                            )
                        })?;
                    let identity =
                        super::StableTargetIdentity::capture(state, Target::Pane(target.clone()))?;
                    Ok((target, pane_id, identity))
                },
            )
            .await;
        let (target, expected_pane_id, identity) = match captured {
            Ok(captured) => captured,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        pause_after_target_resolution(target.session_name(), Some(expected_pane_id)).await;
        if !self.requester_can_write(requester_pid).await {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("client is read-only".to_owned()),
            });
        }

        super::with_expected_stable_target_identities(
            vec![identity],
            None,
            None,
            self.handle_resize_pane(ResizePaneRequest {
                target,
                adjustment: request.adjustment,
            }),
        )
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
