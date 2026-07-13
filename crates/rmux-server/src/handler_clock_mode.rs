use chrono::{Local, Timelike};
use rmux_core::input::mode;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    ClockModeRequest, ClockModeResponse, ErrorResponse, PaneTarget, Response, RmuxError,
    SessionName,
};

use super::pane_support::resolve_input_target;
use super::RequestHandler;
use crate::clock_mode::{next_clock_tick_delay, CLOCK_MODE_NAME};
use crate::handler_support::attached_client_required;
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::HandlerState;
use crate::renderer::{self, ClockPaneRestoreData};

impl RequestHandler {
    pub(super) async fn handle_clock_mode(
        &self,
        requester_pid: u32,
        request: ClockModeRequest,
    ) -> Response {
        let attached_session = {
            let active_attach = self.active_attach.lock().await;
            active_attach.current_session_candidate(requester_pid)
        };
        let target = {
            let state = self.state.lock().await;
            match resolve_input_target(&state, request.target.as_ref(), attached_session.as_ref()) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };

        let session_name = target.session_name().clone();
        let (generation, mode_changed) = {
            let state = self.state.lock().await;
            let transcript = match state.transcript_handle(&target) {
                Ok(transcript) => transcript,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let mut transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            let mode_changed = transcript.pane_mode_name() != Some(CLOCK_MODE_NAME);
            (transcript.enter_clock_mode(), mode_changed)
        };

        if mode_changed {
            self.emit(LifecycleEvent::PaneModeChanged {
                target: target.clone(),
            })
            .await;
        }
        self.refresh_attached_session(&session_name).await;
        self.spawn_clock_mode_timer(target.clone(), generation);

        Response::ClockMode(ClockModeResponse {
            target,
            active: true,
        })
    }

    pub(super) async fn exit_clock_mode(&self, target: &PaneTarget) -> Result<bool, RmuxError> {
        let cleared = {
            let state = self.state.lock().await;
            let transcript = state.transcript_handle(target)?;
            let cleared = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned")
                .clear_clock_mode();
            cleared
        };
        if !cleared {
            return Ok(false);
        }

        let session_name = target.session_name().clone();
        if let Some(frame) = self.clock_mode_restore_frame(target).await? {
            self.send_session_overlay(&session_name, frame, false).await;
        }
        self.sync_automatic_window_name_for_pane_target(target)
            .await;
        self.emit(LifecycleEvent::PaneModeChanged {
            target: target.clone(),
        })
        .await;
        self.refresh_attached_session(&session_name).await;
        Ok(true)
    }

    pub(super) async fn target_is_in_clock_mode(
        &self,
        target: &PaneTarget,
    ) -> Result<bool, RmuxError> {
        let state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let in_clock_mode = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .clock_mode_generation()
            .is_some();
        Ok(in_clock_mode)
    }

    pub(super) async fn refresh_clock_overlays_for_session(&self, session_name: &SessionName) {
        let (pane_indexes, frame) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            let pane_indexes = visible_clock_pane_indexes(&state, session_name);
            let frame = renderer::render_clock_overlay(
                session,
                &state.options,
                &pane_indexes,
                Local::now(),
            );
            (pane_indexes, frame)
        };
        if !pane_indexes.is_empty() && !frame.is_empty() {
            self.send_session_overlay(session_name, frame, true).await;
        }
    }

    pub(super) async fn refresh_clock_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let frame = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(attached_client_required("refresh-client"));
            };
            let pane_indexes = visible_clock_pane_indexes(&state, session_name);
            let frame = renderer::render_clock_overlay(
                session,
                &state.options,
                &pane_indexes,
                Local::now(),
            );
            (!pane_indexes.is_empty() && !frame.is_empty()).then_some(frame)
        };

        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| {
                active.id == expected_attach_id
                    && &active.session_name == session_name
                    && !active.suspended
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
            .ok_or_else(|| attached_client_required("refresh-client"))?;
        let Some(frame) = frame else {
            return Ok(());
        };
        if active.mode_tree.is_some() {
            return Ok(());
        }

        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let overlay =
            OverlayFrame::persistent(frame, active.render_generation, active.overlay_generation);
        active
            .control_tx
            .send(AttachControl::Overlay(overlay))
            .map_err(|_| attached_client_required("refresh-client"))
    }

    fn spawn_clock_mode_timer(&self, target: PaneTarget, generation: u64) {
        let handler = self.clone();
        tokio::spawn(async move {
            let mut last_second = None;
            loop {
                tokio::time::sleep(next_clock_tick_delay()).await;
                let now = Local::now();
                let second = now.second();
                if last_second == Some(second) {
                    continue;
                }
                last_second = Some(second);

                let active = {
                    let state = handler.state.lock().await;
                    let Ok(transcript) = state.transcript_handle(&target) else {
                        return;
                    };
                    let active = transcript
                        .lock()
                        .expect("pane transcript mutex must not be poisoned")
                        .clock_mode_generation()
                        == Some(generation);
                    active
                };
                if !active {
                    return;
                }

                handler
                    .refresh_clock_overlays_for_session(target.session_name())
                    .await;
            }
        });
    }

    async fn clock_mode_restore_frame(
        &self,
        target: &PaneTarget,
    ) -> Result<Option<Vec<u8>>, RmuxError> {
        let state = self.state.lock().await;
        let Some(session) = state.sessions.session(target.session_name()) else {
            return Ok(None);
        };
        if session.active_window_index() != target.window_index() {
            return Ok(None);
        }
        let Some(window) = session.window_at(target.window_index()) else {
            return Ok(None);
        };
        if window.is_zoomed() && window.active_pane_index() != target.pane_index() {
            return Ok(None);
        }

        let lines = state.pane_visible_lines(target)?;
        let cursor_visible = window
            .pane(target.pane_index())
            .and_then(|pane| state.pane_screen_state(target.session_name(), pane.id()))
            .map(|screen| (screen.mode & mode::MODE_CURSOR) != 0)
            .unwrap_or(true);
        Ok(Some(renderer::render_clock_restore_frame(
            session,
            &state.options,
            &[ClockPaneRestoreData {
                pane_index: target.pane_index(),
                lines,
            }],
            cursor_visible,
        )))
    }

    async fn send_session_overlay(
        &self,
        session_name: &SessionName,
        frame: Vec<u8>,
        persistent: bool,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|_, active| {
            if &active.session_name != session_name || active.mode_tree.is_some() {
                return true;
            }

            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let overlay = if persistent {
                OverlayFrame::persistent(
                    frame.clone(),
                    active.render_generation,
                    active.overlay_generation,
                )
            } else {
                OverlayFrame::new(
                    frame.clone(),
                    active.render_generation,
                    active.overlay_generation,
                )
            };
            active
                .control_tx
                .send(AttachControl::Overlay(overlay))
                .is_ok()
        });
    }
}

fn visible_clock_pane_indexes(state: &HandlerState, session_name: &SessionName) -> Vec<u32> {
    let Some(session) = state.sessions.session(session_name) else {
        return Vec::new();
    };
    let window = session.window();
    if window.is_zoomed() {
        return window
            .active_pane()
            .filter(|pane| {
                state
                    .pane_clock_mode_generation(session_name, pane.id())
                    .is_some()
            })
            .map(|pane| vec![pane.index()])
            .unwrap_or_default();
    }

    window
        .panes()
        .iter()
        .filter(|pane| {
            state
                .pane_clock_mode_generation(session_name, pane.id())
                .is_some()
        })
        .map(|pane| pane.index())
        .collect()
}
