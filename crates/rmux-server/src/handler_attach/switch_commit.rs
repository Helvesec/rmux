use std::collections::HashMap;
use std::sync::atomic::Ordering;

use rmux_proto::{
    PaneId, PaneTarget, RmuxError, SessionId, SessionName, TerminalGeometry, WindowId, WindowTarget,
};

use super::resize_policy::ATTACHED_SIZE_RECONCILE_ATTEMPTS;
use super::{
    attach_target_for_session_switch, reset_interactive_attach_state_for_session_switch,
    terminate_overlay_job, AttachSessionSwitchRenderOptions, ClientFlags, RequestHandler,
    ATTACH_CONTROL_BACKLOG_LIMIT,
};
use crate::handler::client_support::SwitchTargetSelection;
use crate::handler::update_environment_from_client;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, SessionTransferSnapshot};

pub(in crate::handler) struct AttachedSwitchCommitRequest {
    pub(in crate::handler) expected_current_session_id: Option<SessionId>,
    pub(in crate::handler) session_name: SessionName,
    pub(in crate::handler) session_id: SessionId,
    pub(in crate::handler) target_selection: Option<SwitchTargetSelection>,
    pub(in crate::handler) terminal_context: OuterTerminalContext,
    pub(in crate::handler) client_geometry: TerminalGeometry,
    pub(in crate::handler) client_flags: ClientFlags,
    pub(in crate::handler) render_stream: bool,
    pub(in crate::handler) attached_count: usize,
    pub(in crate::handler) client_environment: Option<HashMap<String, String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::handler) struct AttachedSwitchCommittedTarget {
    pub(in crate::handler) target: PaneTarget,
    pub(in crate::handler) session_id: SessionId,
    pub(in crate::handler) window_id: WindowId,
    pub(in crate::handler) pane_id: PaneId,
}

pub(in crate::handler) struct AttachedSwitchCommitOutcome {
    pub(in crate::handler) previous_session_name: SessionName,
    pub(in crate::handler) committed_target: AttachedSwitchCommittedTarget,
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct AttachedSwitchPostClosedCheckPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static ATTACHED_SWITCH_POST_CLOSED_CHECK_PAUSE: std::sync::Mutex<
    Option<(u32, std::sync::Arc<AttachedSwitchPostClosedCheckPause>)>,
> = std::sync::Mutex::new(None);

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_attached_switch_post_closed_check_pause(
        &self,
        attach_pid: u32,
    ) -> std::sync::Arc<AttachedSwitchPostClosedCheckPause> {
        let pause = std::sync::Arc::new(AttachedSwitchPostClosedCheckPause::default());
        *ATTACHED_SWITCH_POST_CLOSED_CHECK_PAUSE
            .lock()
            .expect("attached switch delivery pause lock") = Some((attach_pid, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_after_attached_switch_closed_check(&self, attach_pid: u32) {
        let pause = {
            let mut installed = ATTACHED_SWITCH_POST_CLOSED_CHECK_PAUSE
                .lock()
                .expect("attached switch delivery pause lock");
            installed
                .as_ref()
                .is_some_and(|(paused_pid, _)| *paused_pid == attach_pid)
                .then(|| {
                    installed
                        .take()
                        .expect("matching attached switch delivery pause remains installed")
                        .1
                })
        };
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    #[cfg(not(test))]
    async fn pause_after_attached_switch_closed_check(&self, _attach_pid: u32) {}

    pub(in crate::handler) async fn commit_attached_session_switch(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        request: AttachedSwitchCommitRequest,
    ) -> Result<AttachedSwitchCommitOutcome, RmuxError> {
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;

        let incoming_client_size = (!request.client_flags.contains(ClientFlags::IGNORESIZE))
            .then_some(request.client_geometry.size);
        let switch_window_target = request
            .target_selection
            .as_ref()
            .map(SwitchTargetSelection::window_target);

        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let size_selection = match switch_window_target.as_ref() {
                Some(target) => {
                    self.selected_attached_window_size(target, incoming_client_size)
                        .await?
                }
                None => {
                    self.selected_attached_session_size_for_new_client(
                        &request.session_name,
                        request.client_geometry.size,
                        request.client_flags,
                    )
                    .await?
                }
            };
            self.pause_after_attached_size_selection().await;

            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach.by_pid.get(&attach_pid).filter(|active| {
                active.id == expected_attach_id
                    && request
                        .expected_current_session_id
                        .is_none_or(|session_id| active.session_id == session_id)
                    && !active.closing.load(Ordering::SeqCst)
            });
            let Some(active) = active else {
                return Err(crate::handler_support::attached_client_required(
                    "switch-client",
                ));
            };
            // This value must be derived from the attach identity protected by
            // the final state -> active_attach lock pair. Another successful
            // switch can retain the same attach id while changing its session
            // during size selection.
            let switch_changes_session = active.session_name != request.session_name
                || active.session_id != request.session_id;
            if state
                .sessions
                .session(&request.session_name)
                .is_none_or(|session| session.id() != request.session_id)
            {
                return Err(session_not_found(&request.session_name));
            }
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &request.session_name,
                &size_selection,
                switch_window_target.is_none(),
            ) {
                continue;
            }
            if let Some(selection) = request.target_selection.as_ref() {
                selection.validate_for_session_identity(
                    &state,
                    &request.session_name,
                    request.session_id,
                )?;
            }

            let mode_tree_dismiss_plan = if switch_changes_session {
                self.prepare_mode_tree_dismissal_for_committed_switch(
                    &state,
                    &active_attach,
                    attach_pid,
                    expected_attach_id,
                )?
            } else {
                None
            };

            let window_target = switch_window_target.clone().unwrap_or_else(|| {
                let active_window_index = state
                    .sessions
                    .session(&request.session_name)
                    .expect("validated switch session remains present")
                    .active_window_index();
                WindowTarget::with_window(request.session_name.clone(), active_window_index)
            });
            let control_is_closed = active_attach
                .by_pid
                .get(&attach_pid)
                .expect("validated attached identity remains present")
                .control_tx
                .is_closed();
            if control_is_closed {
                // This deterministic closed-receiver case must not reach any
                // target model or PTY resize. A receiver can still close in
                // the narrow interval after this check; the send-failure path
                // below retains the runtime rollback for that residual race.
                let removed = active_attach
                    .remove_attached_client(attach_pid)
                    .expect("closed attached delivery removes the validated identity");
                removed.closing.store(true, Ordering::SeqCst);
                self.bump_active_attach_epoch();
                drop(active_attach);
                drop(state);
                terminate_overlay_job(removed.overlay);
                return Err(crate::handler_support::attached_client_required(
                    "switch-client",
                ));
            }
            self.pause_after_attached_switch_closed_check(attach_pid)
                .await;
            let backlog_full = active_attach
                .by_pid
                .get(&attach_pid)
                .expect("validated attached identity remains present")
                .control_backlog
                .load(Ordering::Acquire)
                >= ATTACH_CONTROL_BACKLOG_LIMIT;
            if backlog_full {
                let removed = {
                    let active = active_attach
                        .by_pid
                        .get_mut(&attach_pid)
                        .expect("validated attached identity remains present");
                    let _ = active.control_tx.send(AttachControl::Detach);
                    active.closing.store(true, Ordering::SeqCst);
                    active_attach
                        .remove_attached_client(attach_pid)
                        .expect("overloaded attached identity remains present")
                };
                self.bump_active_attach_epoch();
                drop(active_attach);
                drop(state);
                terminate_overlay_job(removed.overlay);
                return Err(RmuxError::Server(
                    "attached client is not draining updates".to_owned(),
                ));
            }

            let target = attach_target_for_session_switch(
                &state,
                &request.session_name,
                AttachSessionSwitchRenderOptions {
                    attached_count: request.attached_count,
                    terminal_context: &request.terminal_context,
                    socket_path: &self.socket_path(),
                    render_stream: request.render_stream,
                    selection: request.target_selection.as_ref(),
                    window_size_override: size_selection
                        .selected_size
                        .map(|size| (window_target.window_index(), size)),
                },
            )?;
            let snapshot = SessionTransferSnapshot::capture(&state);
            if let Some(client_environment) = request.client_environment.as_ref() {
                update_environment_from_client(
                    &mut state,
                    &request.session_name,
                    client_environment,
                );
            }
            if !request.client_flags.contains(ClientFlags::IGNORESIZE) {
                state.set_attached_terminal_pixels(
                    &request.session_name,
                    request.client_geometry.pixels,
                );
            }
            let mutation = state.mutate_session_and_resize_window_terminal(
                &request.session_name,
                window_target.window_index(),
                |session| {
                    session.touch_attached();
                    if let Some(selected_size) = size_selection.selected_size {
                        session.resize_window(window_target.window_index(), selected_size)?;
                    }
                    if let Some(selection) = request.target_selection.as_ref() {
                        selection.apply_to_session(session)?;
                    }
                    Ok(())
                },
            );
            if let Err(error) = mutation {
                snapshot.restore(&mut state);
                return Err(rollback_switch_runtime(&mut state, &window_target, error));
            }
            let Some(committed_session) = state
                .sessions
                .session(&request.session_name)
                .filter(|session| session.id() == request.session_id)
            else {
                snapshot.restore(&mut state);
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached session has no active target".to_owned()),
                ));
            };
            let committed_window_index = committed_session.active_window_index();
            let Some(committed_window) = committed_session.window_at(committed_window_index) else {
                snapshot.restore(&mut state);
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached session has no active window".to_owned()),
                ));
            };
            let Some(committed_pane) = committed_window.active_pane() else {
                snapshot.restore(&mut state);
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached window has no active pane".to_owned()),
                ));
            };
            let committed_target = AttachedSwitchCommittedTarget {
                target: PaneTarget::with_window(
                    request.session_name.clone(),
                    committed_window_index,
                    committed_pane.index(),
                ),
                session_id: committed_session.id(),
                window_id: committed_window.id(),
                pane_id: committed_pane.id(),
            };

            let render_stream_refresh =
                request.render_stream && target.is_coalescible_render_refresh();
            let command = if render_stream_refresh {
                AttachControl::Refresh
            } else {
                AttachControl::switch(target)
            };
            let delivery_failed = {
                let active = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .expect("validated attached identity remains present");
                active.control_backlog.fetch_add(1, Ordering::AcqRel);
                let delivery_failed = active.control_tx.send(command).is_err();
                if delivery_failed {
                    let _ = active.control_backlog.fetch_update(
                        Ordering::AcqRel,
                        Ordering::Acquire,
                        |value| value.checked_sub(1),
                    );
                }
                delivery_failed
            };
            if delivery_failed {
                let removed = active_attach
                    .remove_attached_client(attach_pid)
                    .expect("failed attached delivery removes the validated identity");
                removed.closing.store(true, Ordering::SeqCst);
                self.bump_active_attach_epoch();
                snapshot.restore(&mut state);
                let error = rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    crate::handler_support::attached_client_required("switch-client"),
                );
                drop(active_attach);
                drop(state);
                terminate_overlay_job(removed.overlay);
                return Err(error);
            }

            let mode_tree_effects = mode_tree_dismiss_plan.map(|plan| {
                self.apply_committed_mode_tree_dismissal(
                    &mut state,
                    &mut active_attach,
                    plan,
                    &request.session_name,
                )
            });
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .expect("committed attached identity remains present");
            let previous_session_name = active.session_name.clone();
            let switches_session_identity = active.session_name != request.session_name
                || active.session_id != request.session_id;
            let overlay_to_terminate = switches_session_identity
                .then(|| reset_interactive_attach_state_for_session_switch(active))
                .flatten();
            active.render_generation = active.render_generation.saturating_add(1);
            if render_stream_refresh {
                active.render_refresh_pending = true;
            }
            if switches_session_identity {
                active.last_session = Some(active.session_name.clone());
                active.last_session_id = Some(active.session_id);
            }
            active.session_name = request.session_name.clone();
            active.session_id = request.session_id;

            drop(active_attach);
            drop(state);
            if let Some(effects) = mode_tree_effects {
                self.finish_committed_mode_tree_dismissal(effects).await;
            }
            terminate_overlay_job(overlay_to_terminate);
            return Ok(AttachedSwitchCommitOutcome {
                previous_session_name,
                committed_target,
            });
        }

        Err(RmuxError::Server(format!(
            "session {} active window changed during attached-size selection",
            request.session_name
        )))
    }
}

fn rollback_switch_runtime(
    state: &mut crate::pane_terminals::HandlerState,
    target: &WindowTarget,
    cause: RmuxError,
) -> RmuxError {
    match state.resize_window_terminal_runtime(target.session_name(), target.window_index()) {
        Ok(()) => cause,
        Err(rollback_error) => RmuxError::Server(format!(
            "failed to roll back switch-client runtime after {cause}: {rollback_error}"
        )),
    }
}
