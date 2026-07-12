use std::{io, time::Duration};

use rmux_core::{key_code_lookup_bits, key_code_to_bytes};
use rmux_proto::{
    DisplayPanesResponse, ErrorResponse, OptionName, PaneTarget, Request, Response, RmuxError,
    SelectPaneRequest, Target, WindowTarget,
};

use super::super::{
    attach_support::{attach_target_for_session, DisplayPanesClientState, DisplayPanesLabel},
    prompt_support::{substitute_prompt_template, PromptInputEvent},
    scripting_support::command_parser_from_state,
    scripting_support::QueueExecutionContext,
    RequestHandler,
};
use super::{decode_prompt_input_event, io_other, retain_partial_attached_control_input};
use crate::key_table::{
    decode_attached_key, matches_prefix_key, session_option_key, AttachedKeyDecode,
};
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::session_not_found;
use crate::renderer;

#[path = "display_panes/input_state.rs"]
mod input_state;

use self::input_state::{update_display_panes_state, DisplayPanesOutcome};

const DEFAULT_DISPLAY_PANES_TEMPLATE: &str = "select-pane -t '%%'";

impl RequestHandler {
    pub(in crate::handler) async fn handle_display_panes(
        &self,
        requester_pid: u32,
        request: rmux_proto::DisplayPanesRequest,
    ) -> Response {
        let attach_pid = match self
            .resolve_target_attach_client_pid(
                requester_pid,
                request.target_client.as_deref(),
                "display-panes",
            )
            .await
        {
            Ok(attach_pid) => attach_pid,
            Err(error) => {
                return Response::Error(ErrorResponse {
                    error: if request.target_client.is_none() {
                        RmuxError::Message("no current client".to_owned())
                    } else {
                        display_panes_client_error(error)
                    },
                });
            }
        };
        let session_name = match self
            .attached_session_name_for_command(attach_pid, "display-panes")
            .await
        {
            Ok(session_name) => session_name,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let (response, overlay_frame, clear_frame, duration) = {
            let state = self.state.lock().await;
            match state.sessions.session(&session_name) {
                Some(session) => {
                    let overlay_frame =
                        renderer::render_display_panes_overlay(session, &state.options);
                    let clear_frame = renderer::render_display_panes_clear(session, &state.options);
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

        let armed_state = if let Response::DisplayPanes(success) = &response {
            self.arm_display_panes_state(
                attach_pid,
                &session_name,
                success.target.clone(),
                clear_frame.clone(),
                request.no_command,
                request.template.clone(),
            )
            .await
        } else {
            None
        };
        let Some((attach_pid, state_id)) = armed_state else {
            return Response::Error(ErrorResponse {
                error: RmuxError::Message("no current client".to_owned()),
            });
        };

        if !self
            .send_attached_display_panes_overlay_now(
                attach_pid,
                &session_name,
                overlay_frame,
                clear_frame,
            )
            .await
        {
            let _ = self
                .clear_display_panes_state(attach_pid, Some(state_id), false)
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
                .clear_display_panes_state(attach_pid, Some(state_id), true)
                .await;
        } else {
            self.schedule_display_panes_timeout(attach_pid, state_id, duration);
        }

        response
    }

    async fn arm_display_panes_state(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
        window: WindowTarget,
        clear_frame: Vec<u8>,
        no_command: bool,
        template: Option<String>,
    ) -> Option<(u32, u64)> {
        let labels = {
            let state = self.state.lock().await;
            state
                .sessions
                .session(session_name)
                .map(|session| renderer::display_pane_targets(session, &state.options))
                .unwrap_or_default()
        };
        let template = if no_command {
            None
        } else {
            Some(template.unwrap_or_else(|| DEFAULT_DISPLAY_PANES_TEMPLATE.to_owned()))
        };
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach.by_pid.get_mut(&attach_pid)?;
        if active.session_name != *session_name || active.suspended {
            return None;
        }
        active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
        let id = active.display_panes_state_id;
        active.display_panes = Some(DisplayPanesClientState {
            id,
            window,
            labels: labels
                .iter()
                .map(|label| DisplayPanesLabel {
                    label: label.label.clone(),
                    target: label.target.clone(),
                    target_string: label.target_string.clone(),
                })
                .collect(),
            input: String::new(),
            template,
            clear_frame,
        });
        Some((attach_pid, id))
    }

    async fn send_attached_display_panes_overlay_now(
        &self,
        attach_pid: u32,
        session_name: &rmux_proto::SessionName,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
    ) -> bool {
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return false;
        };
        if active.session_name != *session_name || active.suspended {
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

        let delivered = active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                frame,
                render_generation,
                overlay_generation,
            )))
            .is_ok();
        if !delivered {
            active_attach.by_pid.remove(&attach_pid);
        }
        delivered
    }

    fn schedule_display_panes_timeout(&self, attach_pid: u32, state_id: u64, duration: Duration) {
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let _ = handler
                .clear_display_panes_state(attach_pid, Some(state_id), true)
                .await;
        });
    }

    async fn clear_display_panes_state(
        &self,
        attach_pid: u32,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        let cleared = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(false);
            };
            let matches_state = active
                .display_panes
                .as_ref()
                .is_some_and(|state| expected_state_id.map(|id| id == state.id).unwrap_or(true));
            if !matches_state {
                return Ok(false);
            }
            let state = active
                .display_panes
                .take()
                .expect("display-panes state exists when matched");
            let overlay = if send_clear {
                active.overlay_generation = active.overlay_generation.saturating_add(1);
                Some((
                    active.control_tx.clone(),
                    active.render_generation,
                    active.overlay_generation,
                    state.clear_frame,
                ))
            } else {
                None
            };
            Some(overlay)
        };

        let cleared_exists = cleared.is_some();
        if let Some(Some((
            control_tx,
            render_generation,
            overlay_generation,
            fallback_clear_frame,
        ))) = cleared
        {
            if self.attached_persistent_overlay_active(attach_pid).await {
                if !self.restore_mode_tree_overlay_if_active(attach_pid).await? {
                    let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
                }
            } else {
                let clear_frame = self
                    .render_attached_display_panes_clear_frame(attach_pid)
                    .await
                    .unwrap_or(fallback_clear_frame);
                let overlay = OverlayFrame::new(clear_frame, render_generation, overlay_generation);
                let _ = control_tx.send(AttachControl::Overlay(overlay));
            }
            let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
        }

        Ok(cleared_exists)
    }

    async fn render_attached_display_panes_clear_frame(&self, attach_pid: u32) -> Option<Vec<u8>> {
        let (session_name, terminal_context) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach.by_pid.get(&attach_pid)?;
            (active.session_name.clone(), active.terminal_context.clone())
        };
        let attached_count = self.attached_count(&session_name).await;
        let state = self.state.lock().await;
        let session = state.sessions.session(&session_name)?;
        let target = attach_target_for_session(
            &state,
            &session_name,
            attached_count,
            &terminal_context,
            &self.socket_path(),
        )
        .ok()?;
        Some(renderer::render_display_panes_clear_with_base(
            session,
            &state.options,
            &target.render_frame,
        ))
    }

    pub(super) async fn display_panes_active(&self, attach_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&attach_pid)
            .is_some_and(|active| active.display_panes.is_some())
    }

    async fn display_panes_prefix_input(
        &self,
        attach_pid: u32,
        input: &[u8],
    ) -> Result<DisplayPanesPrefixInput, RmuxError> {
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .map(|active| active.session_name.clone())
        };
        let Some(session_name) = session_name else {
            return Ok(DisplayPanesPrefixInput::Other);
        };

        let (prefix_key, prefix2_key, prefix_bytes, prefix2_bytes, backspace) = {
            let state = self.state.lock().await;
            let prefix_key = session_option_key(&state, &session_name, OptionName::Prefix);
            let prefix2_key = session_option_key(&state, &session_name, OptionName::Prefix2);
            let prefix_bytes = prefix_key.and_then(key_code_to_bytes);
            let prefix2_bytes = prefix2_key.and_then(key_code_to_bytes);
            let backspace = state
                .options
                .resolve(None, OptionName::Backspace)
                .and_then(rmux_core::key_string_lookup_string)
                .and_then(key_code_to_bytes)
                .and_then(|bytes| (bytes.len() == 1).then_some(bytes[0]));
            (
                prefix_key,
                prefix2_key,
                prefix_bytes,
                prefix2_bytes,
                backspace,
            )
        };

        for prefix in [prefix_bytes.as_deref(), prefix2_bytes.as_deref()]
            .into_iter()
            .flatten()
        {
            if input == prefix {
                return Ok(DisplayPanesPrefixInput::Prefix);
            }
            if !input.is_empty() && prefix.starts_with(input) {
                return Ok(DisplayPanesPrefixInput::Partial);
            }
        }

        match decode_attached_key(input, backspace) {
            AttachedKeyDecode::Matched { key, .. }
                if matches_prefix_key(key_code_lookup_bits(key), prefix_key, prefix2_key) =>
            {
                Ok(DisplayPanesPrefixInput::Prefix)
            }
            _ => Ok(DisplayPanesPrefixInput::Other),
        }
    }

    pub(super) async fn handle_attached_display_panes_input(
        &self,
        attach_pid: u32,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        // See `handle_attached_prompt_input` for the paste-marker strip
        // rationale — display-panes decodes input as digit/letter keys and a
        // bracketed-paste envelope would cancel the overlay and leak the body.
        // The one-byte tmux prefix (e.g. C-b = 0x02) survives the strip so the
        // prefix passthrough path still triggers. Strip the CONCATENATED
        // buffer so a marker that straddles the pending_input / bytes seam
        // still collapses.
        use super::strip_bracketed_paste_markers;
        pending_input.extend_from_slice(bytes);
        let scrubbed = strip_bracketed_paste_markers(pending_input);
        pending_input.clear();
        pending_input.extend_from_slice(&scrubbed);
        match self
            .display_panes_prefix_input(attach_pid, pending_input)
            .await
            .map_err(io_other)?
        {
            DisplayPanesPrefixInput::Prefix => {
                self.clear_display_panes_state(attach_pid, None, true)
                    .await
                    .map_err(io_other)?;
                return Ok(Some(std::mem::take(pending_input)));
            }
            DisplayPanesPrefixInput::Partial => {
                retain_partial_attached_control_input("display-panes prefix", pending_input)?;
                return Ok(None);
            }
            DisplayPanesPrefixInput::Other => {}
        }

        loop {
            let Some((event, consumed)) = decode_prompt_input_event(pending_input) else {
                retain_partial_attached_control_input("display-panes prompt input", pending_input)?;
                return Ok(None);
            };
            pending_input.drain(..consumed);
            self.handle_display_panes_event(attach_pid, event)
                .await
                .map_err(io_other)?;
            if !self.display_panes_active(attach_pid).await {
                break;
            }
        }

        Ok((!pending_input.is_empty()).then(|| std::mem::take(pending_input)))
    }

    async fn handle_display_panes_event(
        &self,
        attach_pid: u32,
        event: PromptInputEvent,
    ) -> Result<(), RmuxError> {
        let action = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(());
            };
            let Some(state) = active.display_panes.as_mut() else {
                return Ok(());
            };

            match update_display_panes_state(state, event) {
                DisplayPanesOutcome::Stay => None,
                DisplayPanesOutcome::Close => {
                    let state = active
                        .display_panes
                        .take()
                        .expect("display-panes state exists");
                    active.overlay_generation = active.overlay_generation.saturating_add(1);
                    Some(DisplayPanesAction::Clear {
                        attach_pid,
                        control_tx: active.control_tx.clone(),
                        render_generation: active.render_generation,
                        overlay_generation: active.overlay_generation,
                        fallback_clear_frame: state.clear_frame,
                    })
                }
                DisplayPanesOutcome::Select(label) => {
                    let state = active
                        .display_panes
                        .take()
                        .expect("display-panes state exists");
                    active.overlay_generation = active.overlay_generation.saturating_add(1);
                    Some(DisplayPanesAction::Execute {
                        attach_pid,
                        control_tx: active.control_tx.clone(),
                        render_generation: active.render_generation,
                        overlay_generation: active.overlay_generation,
                        fallback_clear_frame: state.clear_frame,
                        target: label.target,
                        target_string: label.target_string,
                        template: state.template,
                    })
                }
            }
        };

        match action {
            None => {}
            Some(DisplayPanesAction::Clear {
                attach_pid,
                control_tx,
                render_generation,
                overlay_generation,
                fallback_clear_frame,
            }) => {
                if self.attached_persistent_overlay_active(attach_pid).await {
                    if !self.restore_mode_tree_overlay_if_active(attach_pid).await? {
                        let _ = self.refresh_mode_tree_overlay_if_active(attach_pid).await;
                    }
                } else {
                    let clear_frame = self
                        .render_attached_display_panes_clear_frame(attach_pid)
                        .await
                        .unwrap_or(fallback_clear_frame);
                    let overlay =
                        OverlayFrame::new(clear_frame, render_generation, overlay_generation);
                    let _ = control_tx.send(AttachControl::Overlay(overlay));
                }
                let _ = self.refresh_interactive_overlay_if_active(attach_pid).await;
            }
            Some(DisplayPanesAction::Execute {
                attach_pid,
                control_tx,
                render_generation,
                overlay_generation,
                fallback_clear_frame,
                target,
                target_string,
                template,
            }) => {
                let clear_frame = self
                    .render_attached_display_panes_clear_frame(attach_pid)
                    .await
                    .unwrap_or(fallback_clear_frame);
                let overlay = OverlayFrame::new(clear_frame, render_generation, overlay_generation);
                let _ = control_tx.send(AttachControl::Overlay(overlay));
                if let Some(template) = template {
                    if template == DEFAULT_DISPLAY_PANES_TEMPLATE {
                        let outcome = self
                            .dispatch_for_connection(
                                attach_pid,
                                u64::from(attach_pid),
                                Request::SelectPane(Box::new(SelectPaneRequest {
                                    target,
                                    title: None,
                                    style: None,
                                    input_disabled: None,
                                    preserve_zoom: false,
                                })),
                            )
                            .await;
                        match outcome.response {
                            Response::SelectPane(_) => return Ok(()),
                            Response::Error(ErrorResponse { error }) => return Err(error),
                            _ => {
                                return Err(RmuxError::Server(
                                    "display-panes select-pane returned unexpected response"
                                        .to_owned(),
                                ));
                            }
                        }
                    }
                    let substituted = substitute_prompt_template(&template, &[target_string]);
                    let parser = {
                        let state = self.state.lock().await;
                        command_parser_from_state(&state)
                    };
                    let parsed = parser.parse_one_group(&substituted).map_err(|error| {
                        RmuxError::Server(format!(
                            "display-panes command parse failed: {}",
                            error.message()
                        ))
                    })?;
                    let context = QueueExecutionContext::without_caller_cwd()
                        .with_current_target(Some(Target::Pane(target)));
                    let _ = self
                        .execute_parsed_commands(attach_pid, parsed, context)
                        .await?;
                }
            }
        }

        Ok(())
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

enum DisplayPanesAction {
    Clear {
        attach_pid: u32,
        control_tx: tokio::sync::mpsc::UnboundedSender<AttachControl>,
        render_generation: u64,
        overlay_generation: u64,
        fallback_clear_frame: Vec<u8>,
    },
    Execute {
        attach_pid: u32,
        control_tx: tokio::sync::mpsc::UnboundedSender<AttachControl>,
        render_generation: u64,
        overlay_generation: u64,
        fallback_clear_frame: Vec<u8>,
        target: PaneTarget,
        target_string: String,
        template: Option<String>,
    },
}

enum DisplayPanesPrefixInput {
    Prefix,
    Partial,
    Other,
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
