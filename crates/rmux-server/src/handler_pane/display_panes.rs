use std::{io, time::Duration};

use rmux_core::{key_code_lookup_bits, key_code_to_bytes};
use rmux_proto::{
    DisplayPanesResponse, ErrorResponse, OptionName, PaneTarget, Request, Response, RmuxError,
    SelectPaneRequest, Target, WindowTarget,
};

use super::super::{
    attach_support::{
        attach_target_for_session, ActiveAttachIdentity, DisplayPanesClientState, DisplayPanesLabel,
    },
    prompt_support::{substitute_prompt_template, PromptInputEvent},
    scripting_support::command_parser_from_state,
    scripting_support::QueueExecutionContext,
    RequestHandler,
};
use super::{
    decode_prompt_input_event, io_other, retain_partial_attached_escape_input,
    strip_bracketed_paste_markers_after_append,
};
use crate::handler_support::attached_client_required;
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

struct DisplayPanesArmRequest<'a> {
    attach_pid: u32,
    identity: Option<ActiveAttachIdentity>,
    session_name: &'a rmux_proto::SessionName,
    window: WindowTarget,
    clear_frame: Vec<u8>,
    no_command: bool,
    template: Option<String>,
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
        let attach_pid = match identity {
            Some(identity) => identity.attach_pid(),
            None => match self
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
            },
        };
        let session_name = match identity {
            Some(identity) => self.attached_session_name_for_identity(identity).await,
            None => {
                self.attached_session_name_for_command(attach_pid, "display-panes")
                    .await
            }
        };
        let session_name = match session_name {
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
            self.arm_display_panes_state(DisplayPanesArmRequest {
                attach_pid,
                identity,
                session_name: &session_name,
                window: success.target.clone(),
                clear_frame: clear_frame.clone(),
                no_command: request.no_command,
                template: request.template.clone(),
            })
            .await
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
    ) -> Option<(ActiveAttachIdentity, u64)> {
        let DisplayPanesArmRequest {
            attach_pid,
            identity,
            session_name,
            window,
            clear_frame,
            no_command,
            template,
        } = request;
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
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))?;
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
        Some((active.identity(attach_pid), id))
    }

    async fn send_attached_display_panes_overlay_now(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
        session_name: &rmux_proto::SessionName,
        overlay_frame: Vec<u8>,
        clear_frame: Vec<u8>,
    ) -> bool {
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

        let delivered = active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                frame,
                render_generation,
                overlay_generation,
            )))
            .is_ok();
        if !delivered {
            active_attach.by_pid.remove(&identity.attach_pid());
        }
        delivered
    }

    pub(in crate::handler) async fn refresh_display_panes_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_display_panes_overlay_with_expected_identity(
            attach_pid,
            expected_attach_id,
            session_name,
            None,
        )
        .await
    }

    pub(in crate::handler) async fn refresh_display_panes_overlay_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) -> Result<(), RmuxError> {
        self.refresh_display_panes_overlay_with_expected_identity(
            identity.attach_pid(),
            identity.attach_id(),
            session_name,
            Some(session_id),
        )
        .await
    }

    async fn refresh_display_panes_overlay_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
    ) -> Result<(), RmuxError> {
        let expected_state_id = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == expected_attach_id
                        && &active.session_name == session_name
                        && expected_session_id.is_none_or(|expected| active.session_id == expected)
                        && (expected_session_id.is_none() || active.prompt.is_none())
                        && !active.suspended
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            let Some(display_panes) = active.display_panes.as_ref() else {
                return Ok(());
            };
            display_panes.id
        };
        let (overlay_frame, clear_frame) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            if expected_session_id.is_some_and(|expected| session.id() != expected) {
                return Err(attached_client_required("refresh-client"));
            }
            (
                renderer::render_display_panes_overlay(session, &state.options),
                renderer::render_display_panes_clear(session, &state.options),
            )
        };

        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| {
                active.id == expected_attach_id
                    && &active.session_name == session_name
                    && expected_session_id.is_none_or(|expected| active.session_id == expected)
                    && (expected_session_id.is_none() || active.prompt.is_none())
                    && !active.suspended
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| attached_client_required("refresh-client"))?;
        if active
            .display_panes
            .as_ref()
            .is_none_or(|display_panes| display_panes.id != expected_state_id)
        {
            return Ok(());
        }

        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let mut frame =
            if active.mode_tree.is_some() || active.overlay.is_some() || active.render_stream {
                Vec::new()
            } else {
                clear_frame
            };
        frame.extend_from_slice(&overlay_frame);
        active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                frame,
                active.render_generation,
                active.overlay_generation,
            )))
            .map_err(|_| attached_client_required("refresh-client"))
    }

    fn schedule_display_panes_timeout(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
        duration: Duration,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let _ = handler
                .clear_display_panes_state_for_identity(identity, Some(state_id), true)
                .await;
        });
    }

    async fn clear_display_panes_state(
        &self,
        attach_pid: u32,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_inner(attach_pid, None, expected_state_id, send_clear)
            .await
    }

    async fn clear_display_panes_state_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_inner(
            identity.attach_pid(),
            Some(identity),
            expected_state_id,
            send_clear,
        )
        .await
    }

    #[cfg(test)]
    pub(in crate::handler) async fn expire_display_panes_for_identity_for_test(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_for_identity(identity, Some(state_id), true)
            .await
    }

    async fn clear_display_panes_state_inner(
        &self,
        attach_pid: u32,
        expected_identity: Option<ActiveAttachIdentity>,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        let (captured_identity, captured_state_id, fallback_clear_frame) = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid) else {
                return Ok(false);
            };
            if expected_identity.is_some_and(|identity| !identity.matches_active(active)) {
                return Ok(false);
            }
            let Some(state) = active.display_panes.as_ref() else {
                return Ok(false);
            };
            if expected_state_id.is_some_and(|id| id != state.id) {
                return Ok(false);
            }
            (
                active.identity(attach_pid),
                state.id,
                state.clear_frame.clone(),
            )
        };
        let clear_frame = if send_clear {
            self.render_attached_display_panes_clear_frame(attach_pid)
                .await
                .unwrap_or(fallback_clear_frame)
        } else {
            Vec::new()
        };

        let clear = {
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(false);
            };
            if !captured_identity.matches_active(active) {
                return Ok(false);
            }
            let matches_state = active
                .display_panes
                .as_ref()
                .is_some_and(|state| state.id == captured_state_id);
            if !matches_state {
                return Ok(false);
            }
            let _state = active
                .display_panes
                .take()
                .expect("display-panes state exists when matched");
            let overlay = if send_clear {
                active.overlay_generation = active.overlay_generation.saturating_add(1);
                let frame = if let Some(overlay) = active.overlay.as_ref() {
                    OverlayFrame::persistent(
                        overlay.render(),
                        active.render_generation,
                        active.overlay_generation,
                    )
                } else if let (Some(_mode_tree), Some(mode_tree_frame)) =
                    (active.mode_tree.as_ref(), active.mode_tree_frame.as_ref())
                {
                    let mut frame = clear_frame;
                    frame.extend_from_slice(mode_tree_frame);
                    OverlayFrame::persistent_with_state(
                        frame,
                        active.render_generation,
                        active.overlay_generation,
                        active.mode_tree_state_id,
                    )
                } else {
                    OverlayFrame::new(
                        clear_frame,
                        active.render_generation,
                        active.overlay_generation,
                    )
                };
                Some((active.control_tx.clone(), frame))
            } else {
                None
            };
            Some(overlay)
        };

        let cleared_exists = clear.is_some();
        if let Some(Some((control_tx, frame))) = clear {
            let _ = control_tx.send(AttachControl::Overlay(frame));
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

    pub(super) async fn display_panes_active_for_identity(
        &self,
        identity: ActiveAttachIdentity,
    ) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active)
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && active.display_panes.is_some()
            })
    }

    async fn display_panes_prefix_input(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        input: &[u8],
    ) -> Result<DisplayPanesPrefixInput, RmuxError> {
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| identity.is_none_or(|identity| identity.matches_active(active)))
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

    pub(super) async fn handle_attached_display_panes_input_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
    ) -> io::Result<Option<Vec<u8>>> {
        self.handle_attached_display_panes_input_with_identity(
            identity.attach_pid(),
            Some(identity),
            pending_input,
            bytes,
        )
        .await
    }

    async fn handle_attached_display_panes_input_with_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
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
        let new_input_at = pending_input.len();
        pending_input.extend_from_slice(bytes);
        strip_bracketed_paste_markers_after_append(pending_input, new_input_at);
        match self
            .display_panes_prefix_input(attach_pid, identity, pending_input)
            .await
            .map_err(io_other)?
        {
            DisplayPanesPrefixInput::Prefix => {
                match identity {
                    Some(identity) => {
                        self.clear_display_panes_state_for_identity(identity, None, true)
                            .await
                    }
                    None => self.clear_display_panes_state(attach_pid, None, true).await,
                }
                .map_err(io_other)?;
                return Ok(Some(std::mem::take(pending_input)));
            }
            DisplayPanesPrefixInput::Partial => {
                retain_partial_attached_escape_input("display-panes prefix", pending_input)?;
                return Ok(None);
            }
            DisplayPanesPrefixInput::Other => {}
        }

        loop {
            let Some((event, consumed)) = decode_prompt_input_event(pending_input) else {
                retain_partial_attached_escape_input("display-panes prompt input", pending_input)?;
                return Ok(None);
            };
            pending_input.drain(..consumed);
            self.handle_display_panes_event(attach_pid, identity, event)
                .await
                .map_err(io_other)?;
            let active = match identity {
                Some(identity) => self.display_panes_active_for_identity(identity).await,
                None => self.display_panes_active(attach_pid).await,
            };
            if !active {
                break;
            }
        }

        Ok((!pending_input.is_empty()).then(|| std::mem::take(pending_input)))
    }

    async fn handle_display_panes_event(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        event: PromptInputEvent,
    ) -> Result<(), RmuxError> {
        let action =
            {
                let mut active_attach = self.active_attach.lock().await;
                let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
                    identity.is_none_or(|identity| identity.matches_active(active))
                }) else {
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
