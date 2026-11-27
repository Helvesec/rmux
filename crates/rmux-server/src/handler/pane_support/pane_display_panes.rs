use std::time::Duration;

use rmux_proto::{
    DisplayPanesResponse, ErrorResponse, PaneTarget, Response, RmuxError, Target, WindowTarget,
};

use super::super::{
    attach_support::{
        append_transient_message_frame, ActiveAttachIdentity, DisplayPanesClientState,
        DisplayPanesLabel,
    },
    focus_support::prepare_server_focus_effects,
    scripting_support::{queued_command_context, QueueExecutionContext},
    RequestHandler, RequesterOrigin, StableTargetIdentity,
};
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::session_not_found;
use crate::renderer;

mod input_dispatch;
mod input_state;
mod overlay_lifecycle;
mod target;

const DEFAULT_DISPLAY_PANES_TEMPLATE: &str = "select-pane -t '%%'";

struct DisplayPanesArmRequest<'a> {
    attach_pid: u32,
    origin: RequesterOrigin,
    identity: Option<ActiveAttachIdentity>,
    session_name: &'a rmux_proto::SessionName,
    window: WindowTarget,
    clear_frame: Vec<u8>,
    no_command: bool,
    template: Option<String>,
    command_context: QueueExecutionContext,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_display_panes(
        &self,
        requester_pid: u32,
        request: rmux_proto::DisplayPanesRequest,
    ) -> Response {
        self.handle_display_panes_with_identity(requester_pid, None, request)
            .await
    }

    pub(in crate::handler) async fn handle_display_panes_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        requester_pid: u32,
        request: rmux_proto::DisplayPanesRequest,
    ) -> Response {
        self.handle_display_panes_with_identity(requester_pid, Some(identity), request)
            .await
    }

    async fn handle_display_panes_with_identity(
        &self,
        requester_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        request: rmux_proto::DisplayPanesRequest,
    ) -> Response {
        let origin = self.capture_requester_origin(requester_pid).await;
        let identity = match self
            .display_panes_client_identity(requester_pid, identity, &request)
            .await
        {
            Ok(identity) => identity,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let attach_pid = identity.attach_pid();
        let identity = Some(identity);
        let session_name = request.target.clone();
        let (response, overlay_frame, clear_frame, duration) = {
            let state = self.state.lock().await;
            match state.sessions.session(&session_name) {
                Some(session) => {
                    let overlay_frame =
                        renderer::render_display_panes_overlay(session, &state.options);
                    let clear_frame =
                        renderer::render_display_panes_clear(session, &state.options, &state);
                    (
                        Response::DisplayPanes(DisplayPanesResponse {
                            target: WindowTarget::with_window(
                                session_name.clone(),
                                session.active_window_index(),
                            ),
                            pane_count: renderer::display_panes_label_count(
                                session,
                                &state.options,
                            ),
                        }),
                        overlay_frame,
                        clear_frame,
                        request.duration_ms.map_or_else(
                            || display_panes_time(&state.options, &session_name),
                            |ms| Duration::from_millis(ms.max(1)),
                        ),
                    )
                }
                None => {
                    return Response::Error(ErrorResponse {
                        error: session_not_found(&session_name),
                    });
                }
            }
        };

        let command_context =
            queued_command_context().unwrap_or_else(QueueExecutionContext::without_caller_cwd);
        let command_context = if request.non_blocking {
            command_context
                .with_mouse_target(None)
                .without_mouse_event()
        } else {
            command_context
        };
        let armed_state = if let Response::DisplayPanes(success) = &response {
            match self
                .arm_display_panes_state(DisplayPanesArmRequest {
                    attach_pid,
                    origin,
                    identity,
                    session_name: &session_name,
                    window: success.target.clone(),
                    clear_frame: clear_frame.clone(),
                    no_command: request.no_command,
                    template: request.template.clone(),
                    command_context,
                })
                .await
            {
                Ok(armed) => armed,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };
        let Some((attach_identity, state_id)) = armed_state else {
            return Response::Error(ErrorResponse {
                error: RmuxError::Message("no current client".to_owned()),
            });
        };

        if !self
            .send_attached_display_panes_overlay_now(
                attach_identity,
                state_id,
                &session_name,
                overlay_frame,
                clear_frame,
            )
            .await
        {
            let _ = self
                .clear_display_panes_state_for_identity(attach_identity, Some(state_id), false)
                .await;
            return Response::Error(ErrorResponse {
                error: RmuxError::Message("no current client".to_owned()),
            });
        }

        if let Response::DisplayPanes(success) = &response {
            let active_pane = {
                let state = self.state.lock().await;
                state
                    .sessions
                    .session(&session_name)
                    .and_then(|session| session.window_at(success.target.window_index()))
                    .map(|window| window.active_pane_index())
            };
            if let Some(active_pane) = active_pane {
                self.emit(rmux_core::LifecycleEvent::PaneModeChanged {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        success.target.window_index(),
                        active_pane,
                    ),
                })
                .await;
            }
        }

        if !request.non_blocking {
            tokio::time::sleep(duration).await;
            let _ = self
                .clear_display_panes_state_for_identity(attach_identity, Some(state_id), true)
                .await;
        } else {
            self.schedule_display_panes_timeout(attach_identity, state_id, duration);
        }

        response
    }

    async fn arm_display_panes_state(
        &self,
        request: DisplayPanesArmRequest<'_>,
    ) -> Result<Option<(ActiveAttachIdentity, u64)>, RmuxError> {
        let DisplayPanesArmRequest {
            attach_pid,
            origin,
            identity,
            session_name,
            window,
            clear_frame,
            no_command,
            template,
            command_context,
        } = request;
        let labels = {
            let mut state = self.state.lock().await;
            let rendered = state
                .sessions
                .session(session_name)
                .map(|session| renderer::display_pane_targets(session, &state.options))
                .unwrap_or_default();
            rendered
                .into_iter()
                .map(|label| {
                    let target_identity = StableTargetIdentity::capture(
                        &mut state,
                        Target::Pane(label.target.clone()),
                    )?;
                    Ok(DisplayPanesLabel {
                        label: label.label,
                        target: label.target,
                        target_string: label.target_string,
                        target_identity,
                    })
                })
                .collect::<Result<Vec<_>, RmuxError>>()?
        };
        let template = if no_command {
            None
        } else {
            Some(template.unwrap_or_else(|| DEFAULT_DISPLAY_PANES_TEMPLATE.to_owned()))
        };
        let (armed, focus_effects) = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)));
            let Some(active) = active else {
                return Ok(None);
            };
            if active.session_name != *session_name || active.suspended {
                return Ok(None);
            }
            active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
            let id = active.display_panes_state_id;
            active.display_panes = Some(DisplayPanesClientState {
                id,
                origin,
                command_context,
                window,
                labels,
                input: String::new(),
                template,
                clear_frame,
            });
            let armed = Some((active.identity(attach_pid), id));
            let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
            (armed, focus_effects)
        };
        let _ = focus_effects.execute(self).await;
        Ok(armed)
    }

    async fn send_attached_display_panes_overlay_now(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
        session_name: &rmux_proto::SessionName,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
    ) -> bool {
        let (delivered, focus_effects) = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&identity.attach_pid()) else {
                return false;
            };
            if !identity.matches_active(active)
                || active.session_name != *session_name
                || active.suspended
                || active
                    .display_panes
                    .as_ref()
                    .is_none_or(|state| state.id != state_id)
            {
                return false;
            }

            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let render_generation = active.render_generation;
            let overlay_generation = active.overlay_generation;
            let mut frame =
                if active.mode_tree.is_some() || active.overlay.is_some() || active.render_stream {
                    Vec::new()
                } else {
                    clear_frame
                };
            frame.extend_from_slice(&overlay_frame);
            append_transient_message_frame(active, &mut frame);

            let delivered = active
                .control_tx
                .send(AttachControl::Overlay(OverlayFrame::new(
                    frame,
                    render_generation,
                    overlay_generation,
                )))
                .is_ok();
            let focus_effects = if delivered {
                None
            } else {
                active_attach.by_pid.remove(&identity.attach_pid());
                active_attach.forget_attached_client_windows(identity.attach_pid());
                Some(prepare_server_focus_effects(&mut state, &mut active_attach))
            };
            (delivered, focus_effects)
        };
        if let Some(focus_effects) = focus_effects {
            let _ = focus_effects.execute(self).await;
        }
        delivered
    }
}

fn display_panes_client_error(error: RmuxError) -> RmuxError {
    match error {
        RmuxError::Server(message) if message.starts_with("can't find client: ") => {
            RmuxError::Message(message)
        }
        error => error,
    }
}

pub(super) fn display_panes_time(
    options: &rmux_core::OptionStore,
    session_name: &rmux_proto::SessionName,
) -> Duration {
    Duration::from_millis(
        options
            .resolve(Some(session_name), rmux_proto::OptionName::DisplayPanesTime)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(1_000)
            .max(1),
    )
}
