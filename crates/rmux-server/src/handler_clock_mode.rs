use chrono::{Local, Timelike};
use rmux_core::input::mode;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    ClockModeRequest, ClockModeResponse, ErrorResponse, PaneId, PaneTarget, Response, RmuxError,
    SessionId, SessionName, WindowId, WindowTarget,
};

use super::pane_support::resolve_input_target;
use super::RequestHandler;
use crate::clock_mode::{next_clock_tick_delay, CLOCK_MODE_NAME};
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler_support::attached_client_required;
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::HandlerState;
use crate::renderer::{self, ClockPaneRenderData, ClockPaneRestoreData};

#[cfg(test)]
#[path = "handler_clock_mode/identity_test_pause.rs"]
mod identity_test_pause;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ClockModePaneIdentity {
    session_id: SessionId,
    window_id: WindowId,
    pane_id: PaneId,
}

struct ClockModeExitEffects {
    pane_identity: ClockModePaneIdentity,
    mode_revision: u64,
    restore_frame: Option<Vec<u8>>,
    lifecycle_event: super::QueuedLifecycleEvent,
    refresh_sessions: Vec<(SessionName, SessionId)>,
}

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
        self.exit_clock_mode_with_identity(target, None).await
    }

    pub(in crate::handler) async fn exit_clock_mode_for_attached_identity(
        &self,
        target: &PaneTarget,
        identity: ActiveAttachIdentity,
        session_id: rmux_proto::SessionId,
    ) -> Result<bool, RmuxError> {
        self.exit_clock_mode_with_identity(target, Some((identity, session_id)))
            .await
    }

    async fn exit_clock_mode_with_identity(
        &self,
        target: &PaneTarget,
        expected_identity: Option<(ActiveAttachIdentity, SessionId)>,
    ) -> Result<bool, RmuxError> {
        let effects = {
            let mut state = self.state.lock().await;
            let active_attach = match expected_identity {
                Some((identity, session_id)) => {
                    let session_matches = state
                        .sessions
                        .session(target.session_name())
                        .is_some_and(|session| session.id() == session_id);
                    if !session_matches {
                        return Ok(false);
                    }
                    let active_attach = self.active_attach.lock().await;
                    let identity_matches = active_attach
                        .by_pid
                        .get(&identity.attach_pid())
                        .is_some_and(|active| {
                            identity.matches_active_session(
                                active,
                                target.session_name(),
                                session_id,
                            ) && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        });
                    if !identity_matches {
                        return Ok(false);
                    }
                    Some(active_attach)
                }
                None => None,
            };
            let transcript = state.transcript_handle(target)?;
            let pane_identity = clock_mode_pane_identity(&state, target).ok_or_else(|| {
                RmuxError::Server("clock-mode pane identity disappeared".to_owned())
            })?;
            let mode_revision = {
                let mut transcript = transcript
                    .lock()
                    .expect("pane transcript mutex must not be poisoned");
                if !transcript.clear_clock_mode() {
                    return Ok(false);
                }
                transcript.pane_mode_revision()
            };
            // Keep the exact attach generation stable through the transcript
            // mutation; a same-PID replacement cannot pass validation and then
            // inherit the stale input's clock-mode exit.
            let effects =
                prepare_clock_mode_exit_effects(&mut state, target, pane_identity, mode_revision)?;
            drop(active_attach);
            effects
        };

        #[cfg(test)]
        self.pause_after_clock_mode_exit_commit_for_test(target)
            .await;

        if let Some(frame) = effects.restore_frame {
            self.send_clock_mode_restore_if_current(
                target,
                effects.pane_identity,
                effects.mode_revision,
                frame,
            )
            .await;
        }
        self.emit_prepared(effects.lifecycle_event).await;
        for (session_name, session_id) in effects.refresh_sessions {
            self.refresh_attached_session_for_session_identity(&session_name, session_id)
                .await;
        }
        Ok(true)
    }

    async fn send_clock_mode_restore_if_current(
        &self,
        target: &PaneTarget,
        expected_identity: ClockModePaneIdentity,
        expected_revision: u64,
        frame: Vec<u8>,
    ) {
        let state = self.state.lock().await;
        if clock_mode_pane_identity(&state, target) != Some(expected_identity) {
            return;
        }
        let revision_is_current = state.transcript_handle(target).is_ok_and(|transcript| {
            let transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            transcript.pane_mode_revision() == expected_revision && !transcript.pane_in_mode()
        });
        if !revision_is_current {
            return;
        }

        #[cfg(test)]
        self.pause_before_clock_mode_restore_commit_for_test(target)
            .await;

        // Keep the pane identity and mode revision stable through publication.
        // A new clock/copy/mode-tree transition must acquire `state` first, so
        // it linearizes either before this validation or after this overlay.
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|_, active| {
            if active.session_name != *target.session_name()
                || active.session_id != expected_identity.session_id
                || active.mode_tree.is_some()
            {
                return true;
            }

            active.overlay_generation = active.overlay_generation.saturating_add(1);
            let overlay = OverlayFrame::new(
                frame.clone(),
                active.render_generation,
                active.overlay_generation,
            );
            active
                .control_tx
                .send(AttachControl::Overlay(overlay))
                .is_ok()
        });
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
        let (panes, frame) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return;
            };
            let panes = visible_clock_panes(&state, session_name);
            let frame =
                renderer::render_clock_overlay(session, &state.options, &panes, Local::now());
            (panes, frame)
        };
        if !panes.is_empty() && !frame.is_empty() {
            self.send_session_overlay(session_name, None, frame, true)
                .await;
        }
    }

    pub(super) async fn refresh_clock_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_clock_overlay_with_expected_identity(
            attach_pid,
            expected_attach_id,
            session_name,
            None,
        )
        .await
    }

    pub(in crate::handler) async fn refresh_clock_overlay_for_session_identity(
        &self,
        identity: super::attach_support::ActiveAttachIdentity,
        session_name: &SessionName,
        session_id: rmux_proto::SessionId,
    ) -> Result<(), RmuxError> {
        self.refresh_clock_overlay_with_expected_identity(
            identity.attach_pid(),
            identity.attach_id(),
            session_name,
            Some(session_id),
        )
        .await
    }

    async fn refresh_clock_overlay_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
    ) -> Result<(), RmuxError> {
        let frame = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(session_name) else {
                return Err(attached_client_required("refresh-client"));
            };
            if expected_session_id.is_some_and(|expected| session.id() != expected) {
                return Err(attached_client_required("refresh-client"));
            }
            let panes = visible_clock_panes(&state, session_name);
            let frame =
                renderer::render_clock_overlay(session, &state.options, &panes, Local::now());
            (!panes.is_empty() && !frame.is_empty()).then_some(frame)
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

    async fn send_session_overlay(
        &self,
        session_name: &SessionName,
        expected_session_id: Option<SessionId>,
        frame: Vec<u8>,
        persistent: bool,
    ) {
        let mut active_attach = self.active_attach.lock().await;
        active_attach.by_pid.retain(|_, active| {
            if &active.session_name != session_name
                || expected_session_id.is_some_and(|expected| active.session_id != expected)
                || active.mode_tree.is_some()
            {
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

fn clock_mode_pane_identity(
    state: &HandlerState,
    target: &PaneTarget,
) -> Option<ClockModePaneIdentity> {
    let session = state.sessions.session(target.session_name())?;
    let window = session.window_at(target.window_index())?;
    let pane = window.pane(target.pane_index())?;
    Some(ClockModePaneIdentity {
        session_id: session.id(),
        window_id: window.id(),
        pane_id: pane.id(),
    })
}

fn prepare_clock_mode_exit_effects(
    state: &mut HandlerState,
    target: &PaneTarget,
    pane_identity: ClockModePaneIdentity,
    mode_revision: u64,
) -> Result<ClockModeExitEffects, RmuxError> {
    let restore_frame = clock_mode_restore_frame_locked(state, target)?;
    let renamed_sessions = RequestHandler::sync_automatic_window_name_for_window_target_locked(
        state,
        &WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        pane_identity.window_id,
    );
    let lifecycle_event = super::prepare_lifecycle_event(
        state,
        &LifecycleEvent::PaneModeChanged {
            target: target.clone(),
        },
    );
    let refresh_sessions =
        clock_mode_refresh_session_identities(state, target.session_name(), renamed_sessions);
    Ok(ClockModeExitEffects {
        pane_identity,
        mode_revision,
        restore_frame,
        lifecycle_event,
        refresh_sessions,
    })
}

fn clock_mode_restore_frame_locked(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<Option<Vec<u8>>, RmuxError> {
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
    let pane_state = window
        .pane(target.pane_index())
        .and_then(|pane| state.pane_screen_state(target.session_name(), pane.id()));
    let cursor_visible = pane_state
        .as_ref()
        .map(|screen| (screen.mode & mode::MODE_CURSOR) != 0)
        .unwrap_or(true);
    let history_size = window
        .pane(target.pane_index())
        .and_then(|pane| state.pane_history_size_stats(target.session_name(), pane.id()))
        .map_or(0, |stats| stats.size);
    Ok(Some(renderer::render_clock_restore_frame(
        session,
        &state.options,
        &[ClockPaneRestoreData {
            pane_index: target.pane_index(),
            lines,
            history_size,
            alternate_on: pane_state.is_some_and(|screen| screen.alternate_on),
        }],
        cursor_visible,
    )))
}

fn clock_mode_refresh_session_identities(
    state: &HandlerState,
    target_session: &SessionName,
    mut renamed_sessions: Vec<SessionName>,
) -> Vec<(SessionName, SessionId)> {
    renamed_sessions.push(target_session.clone());
    renamed_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    renamed_sessions.dedup();
    renamed_sessions
        .into_iter()
        .filter_map(|session_name| {
            state
                .sessions
                .session(&session_name)
                .map(|session| (session_name, session.id()))
        })
        .collect()
}

fn visible_clock_panes(
    state: &HandlerState,
    session_name: &SessionName,
) -> Vec<ClockPaneRenderData> {
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
            .map(|pane| vec![clock_pane_render_data(state, session_name, pane)])
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
        .map(|pane| clock_pane_render_data(state, session_name, pane))
        .collect()
}

fn clock_pane_render_data(
    state: &HandlerState,
    session_name: &SessionName,
    pane: &rmux_core::Pane,
) -> ClockPaneRenderData {
    let history_size = state
        .pane_history_size_stats(session_name, pane.id())
        .map_or(0, |stats| stats.size);
    let alternate_on = state
        .pane_screen_state(session_name, pane.id())
        .is_some_and(|screen| screen.alternate_on);
    ClockPaneRenderData {
        pane_index: pane.index(),
        history_size,
        alternate_on,
    }
}
