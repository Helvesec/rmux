use std::collections::HashMap;
use std::sync::atomic::Ordering;

use rmux_proto::{
    PaneId, PaneTarget, RmuxError, SessionId, SessionName, TerminalGeometry, WindowId, WindowTarget,
};
use tracing::warn;

use super::resize_policy::{IncomingSizeClient, ATTACHED_SIZE_RECONCILE_ATTEMPTS};
use super::{
    attach_target_for_session_switch, reset_interactive_attach_state_for_session_switch,
    terminate_overlay_job, ActiveAttachIdentity, AttachGeneration,
    AttachSessionSwitchRenderOptions, AttachedClientControlOutcome, ClientFlags, RequestHandler,
    ATTACH_CONTROL_BACKLOG_LIMIT,
};
use crate::client_format::sort_client_loop_snapshots;
use crate::handler::client_runtime_support::ListClientSnapshot;
use crate::handler::client_support::SwitchTargetSelection;
use crate::handler::{
    focus_support::{prepare_server_focus_effects, FocusEffects},
    update_environment_from_client, SelectionTargetTransitionSnapshot,
};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::pane_terminals::{session_not_found, SessionTransferSnapshot};

mod publication;

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
    /// The session the client was attached to before the switch. Kept as a
    /// stable identity because the source session can be renamed or destroyed
    /// while the post-switch notifications are published.
    pub(in crate::handler) previous_session_id: SessionId,
    pub(in crate::handler) client_name: String,
    pub(in crate::handler) committed_target: AttachedSwitchCommittedTarget,
}

#[derive(Debug)]
pub(in crate::handler) struct AttachedSwitchCommitFailure {
    error: RmuxError,
    detached_client: Option<AttachedClientControlOutcome>,
    focus_effects: Option<FocusEffects>,
}

impl AttachedSwitchCommitFailure {
    fn detached(
        error: RmuxError,
        detached_client: AttachedClientControlOutcome,
        focus_effects: FocusEffects,
    ) -> Self {
        Self {
            error,
            detached_client: Some(detached_client),
            focus_effects: Some(focus_effects),
        }
    }
}

impl From<RmuxError> for AttachedSwitchCommitFailure {
    fn from(error: RmuxError) -> Self {
        Self {
            error,
            detached_client: None,
            focus_effects: None,
        }
    }
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
    ) -> Result<AttachedSwitchCommitOutcome, AttachedSwitchCommitFailure> {
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;

        // The switching client is still registered under the session it is
        // leaving, so it enters the selection as the incoming client and that
        // exact registration drops out with it. The generation matters: the loop
        // below revalidates it under the final `state` -> `active_attach` lock
        // pair, and a same-pid replacement must keep its own vote until then.
        let displaced = AttachGeneration::new(attach_pid, expected_attach_id);
        // tmux 3.7b's `cmd_switch_client_exec` assigns
        // `s->curw->window->latest = c` before `recalculate_sizes()`, so a client
        // joining a window is that window's newest sizing authority — ahead of
        // clients that arrived after it. One order serves the whole command: the
        // candidate below votes with it and the commit region writes it onto the
        // registration, so the frame this switch sends, the geometry it stores
        // and every later reconcile of the linked family rank the client alike.
        let joined_size_sequence = self.next_client_size_sequence();
        let incoming_client = Some(IncomingSizeClient::joining(
            Some(displaced),
            request.client_geometry.size,
            request.client_flags,
            joined_size_sequence,
        ));
        let switch_window_target = request
            .target_selection
            .as_ref()
            .map(SwitchTargetSelection::window_target);

        for _ in 0..ATTACHED_SIZE_RECONCILE_ATTEMPTS {
            let size_selection = match switch_window_target.as_ref() {
                Some(target) => {
                    self.selected_attached_window_size(target, incoming_client)
                        .await?
                }
                None => {
                    self.selected_attached_session_size(&request.session_name, incoming_client)
                        .await?
                }
            };
            self.pause_after_attached_size_selection().await;
            let mut client_loop_snapshots = self.client_loop_snapshots().await;

            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active_control = self.active_control.lock().await;
            let active = active_attach.by_pid.get(&attach_pid).filter(|active| {
                // The same generation the selection displaced, revalidated
                // before any shared geometry moves or any frame is sent.
                displaced.is(attach_pid, active)
                    && request
                        .expected_current_session_id
                        .is_none_or(|session_id| active.session_id == session_id)
                    && !active.closing.load(Ordering::SeqCst)
            });
            let Some(active) = active else {
                return Err(
                    crate::handler_support::attached_client_required("switch-client").into(),
                );
            };
            // This value must be derived from the attach identity protected by
            // the final state -> active_attach lock pair. Another successful
            // switch can retain the same attach id while changing its session
            // during size selection.
            let switch_changes_session = active.session_name != request.session_name
                || active.session_id != request.session_id;
            // Every session move of an attached client commits here, including
            // the `detach-on-destroy` re-home: a refused re-home leaves the
            // viewer on its destroyed session, whose final close exits it.
            if switch_changes_session && !active.may_leave_session() {
                return Err(RmuxError::Server(
                    "a web share view cannot leave its shared session".to_owned(),
                )
                .into());
            }
            // tmux records the session being left as `c->last_session` only
            // while it is alive: `server_destroy_session` clears it before
            // re-homing a client, and `session_alive()` gates every read.
            let left_session_is_alive = state
                .sessions
                .session(&active.session_name)
                .is_some_and(|session| session.id() == active.session_id);
            let client_name = active.client_name.clone();
            if state
                .sessions
                .session(&request.session_name)
                .is_none_or(|session| session.id() != request.session_id)
            {
                return Err(session_not_found(&request.session_name).into());
            }
            if !self.attached_size_selection_is_current(
                &state,
                &active_attach,
                &active_control,
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
                let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
                drop(active_attach);
                drop(state);
                let session_name = removed.session_name.clone();
                let detached_client = Self::failed_attached_delivery_outcome(removed, session_name);
                return Err(AttachedSwitchCommitFailure::detached(
                    crate::handler_support::attached_client_required("switch-client"),
                    detached_client,
                    focus_effects,
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
                    // The bounded sender turns this over-limit send into the
                    // single accounted terminal Detach sentinel.
                    let _ = active.control_tx.send(AttachControl::Detach);
                    active.closing.store(true, Ordering::SeqCst);
                    active_attach
                        .remove_attached_client(attach_pid)
                        .expect("overloaded attached identity remains present")
                };
                self.bump_active_attach_epoch();
                let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
                drop(active_attach);
                drop(state);
                let session_name = removed.session_name.clone();
                let detached_client = Self::failed_attached_delivery_outcome(removed, session_name);
                return Err(AttachedSwitchCommitFailure::detached(
                    RmuxError::Server("attached client is not draining updates".to_owned()),
                    detached_client,
                    focus_effects,
                ));
            }

            // Captured before the render so the title expands against the
            // client this switch is being delivered to, already carrying the
            // destination this frame commits to: nothing but a later unrelated
            // redraw would correct a stale `#{client_session}`.
            let switching_client = active_attach.by_pid.get(&attach_pid).map(|active| {
                ListClientSnapshot::from_attached_client(attach_pid, active).switched_to_session(
                    &request.session_name,
                    request.session_id,
                    left_session_is_alive,
                )
            });
            if let Some(switching_client) = switching_client.as_ref() {
                if let Some(snapshot) = client_loop_snapshots
                    .iter_mut()
                    .find(|snapshot| snapshot.bindings.name == switching_client.name)
                {
                    *snapshot = switching_client.client_loop_snapshot();
                }
            }
            sort_client_loop_snapshots(&mut client_loop_snapshots);
            let target = attach_target_for_session_switch(
                &state,
                &request.session_name,
                AttachSessionSwitchRenderOptions {
                    attached_count: request.attached_count,
                    key_table: if switch_changes_session {
                        None
                    } else {
                        active.key_table_name.as_deref()
                    },
                    previous_title: active_attach
                        .by_pid
                        .get(&attach_pid)
                        .map(|active| &active.client_title),
                    client: switching_client.as_ref(),
                    terminal_context: &request.terminal_context,
                    socket_path: &self.socket_path(),
                    render_stream: request.render_stream,
                    selection: request.target_selection.as_ref(),
                    window_size_override: size_selection
                        .render_override(window_target.window_index()),
                    client_loop_snapshots: Some(&client_loop_snapshots),
                },
            )?;
            let selection_transition = request
                .target_selection
                .as_ref()
                .map(|selection| {
                    SelectionTargetTransitionSnapshot::capture(&state, selection.window_target())
                })
                .transpose()?;
            let snapshot =
                SessionTransferSnapshot::capture(&state, &[window_target.session_name().clone()])?;
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
            let mutation = state.mutate_switch_window_and_resize_terminal_with_family(
                &window_target,
                Some(request.session_id),
                true,
                |mut switch| {
                    if let Some(selection) = request.target_selection.as_ref() {
                        selection.apply_to_window_switch(&mut switch)?;
                    }
                    size_selection.apply_to_window_switch(&mut switch);
                    Ok(())
                },
            );
            if let Err(error) = mutation {
                snapshot.restore(&mut state)?;
                return Err(rollback_switch_runtime(&mut state, &window_target, error).into());
            }
            let Some(committed_session) = state
                .sessions
                .session(&request.session_name)
                .filter(|session| session.id() == request.session_id)
            else {
                snapshot.restore(&mut state)?;
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached session has no active target".to_owned()),
                )
                .into());
            };
            let committed_window_index = committed_session.active_window_index();
            let Some(committed_window) = committed_session.window_at(committed_window_index) else {
                snapshot.restore(&mut state)?;
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached session has no active window".to_owned()),
                )
                .into());
            };
            let Some(committed_pane) = committed_window.active_pane() else {
                snapshot.restore(&mut state)?;
                return Err(rollback_switch_runtime(
                    &mut state,
                    &window_target,
                    RmuxError::Server("switched attached window has no active pane".to_owned()),
                )
                .into());
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
            let mut refresh_sessions = state
                .window_linked_session_family_list(
                    &request.session_name,
                    window_target.window_index(),
                )
                .into_iter()
                .filter_map(|session_name| {
                    state
                        .sessions
                        .session(&session_name)
                        .map(|session| (session_name, session.id()))
                })
                .collect::<Vec<_>>();
            refresh_sessions.sort_by(|left, right| {
                left.0
                    .as_str()
                    .cmp(right.0.as_str())
                    .then_with(|| left.1.cmp(&right.1))
            });
            refresh_sessions.dedup();

            let render_stream_refresh =
                request.render_stream && target.is_coalescible_render_refresh();
            // Only a delivered switch frame actually carries the title; a
            // coalesced render refresh drops this target and re-renders later.
            let switched_client_title = (!render_stream_refresh)
                .then(|| target.client_title.clone())
                .flatten();
            let command = if render_stream_refresh {
                AttachControl::Refresh
            } else {
                AttachControl::switch(target)
            };
            let delivery_error = {
                let active = active_attach
                    .by_pid
                    .get_mut(&attach_pid)
                    .expect("validated attached identity remains present");
                active.control_tx.send(command).err()
            };
            if let Some(delivery_error) = delivery_error {
                let removed = active_attach
                    .remove_attached_client(attach_pid)
                    .expect("failed attached delivery removes the validated identity");
                removed.closing.store(true, Ordering::SeqCst);
                self.bump_active_attach_epoch();
                snapshot.restore(&mut state)?;
                let delivery_error = if delivery_error.is_full() {
                    RmuxError::Server("attached client is not draining updates".to_owned())
                } else {
                    crate::handler_support::attached_client_required("switch-client")
                };
                let error = rollback_switch_runtime(&mut state, &window_target, delivery_error);
                let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
                drop(active_attach);
                drop(state);
                let session_name = removed.session_name.clone();
                let detached_client = Self::failed_attached_delivery_outcome(removed, session_name);
                return Err(AttachedSwitchCommitFailure::detached(
                    error,
                    detached_client,
                    focus_effects,
                ));
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
            let previous_session_id = active.session_id;
            let switches_session_identity = active.session_name != request.session_name
                || active.session_id != request.session_id;
            if switches_session_identity {
                let previous_key_table = active.key_table_name.take();
                active.key_table_set_at = None;
                active.key_table_generation = active.key_table_generation.wrapping_add(1);
                active.repeat_deadline = None;
                active.repeat_active = false;
                active.last_key = None;
                if let Some(table_name) = previous_key_table {
                    state.key_bindings.unref_table(&table_name);
                }
            }
            let overlay_to_terminate = switches_session_identity
                .then(|| reset_interactive_attach_state_for_session_switch(active))
                .flatten();
            active.render_generation = active.render_generation.saturating_add(1);
            active.remember_client_title(switched_client_title.as_ref());
            if render_stream_refresh {
                active.render_refresh_pending = true;
            }
            if switches_session_identity {
                (active.last_session, active.last_session_id) = if left_session_is_alive {
                    (Some(active.session_name.clone()), Some(active.session_id))
                } else {
                    (None, None)
                };
            }
            active.session_name = request.session_name.clone();
            active.session_id = request.session_id;
            active.reported_focused = true;
            // The order the selection above voted with, now owned by the
            // registration it displaced — the second half of
            // `IncomingSizeClient::joining`'s contract. It lands here rather
            // than earlier so a switch that fails leaves the client's recency
            // exactly where the rolled-back model leaves its geometry.
            active.size_sequence = joined_size_sequence;

            let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
            let selection_events = selection_transition
                .map(|transition| transition.prepare(&mut state))
                .unwrap_or_default();

            drop(active_control);
            drop(active_attach);
            drop(state);
            // Focus and selection tickets were reserved by this commit, in
            // that order. Drain them before mode-tree cleanup prepares any
            // later lifecycle work; otherwise that cleanup can wait forever
            // on a predecessor retained in the not-yet-returned outcome.
            if let Err(error) = focus_effects.execute(self).await {
                warn!("failed to publish switch-client focus transition: {error}");
            }
            for event in selection_events {
                self.emit_prepared(event).await;
            }
            if let Some(effects) = mode_tree_effects {
                self.finish_committed_mode_tree_dismissal(effects).await;
            }
            terminate_overlay_job(overlay_to_terminate);
            let switched_client =
                ActiveAttachIdentity::new(attach_pid, expected_attach_id, request.session_id);
            for (refresh_session_name, refresh_session_id) in refresh_sessions {
                self.refresh_attached_session_for_session_identity_except(
                    &refresh_session_name,
                    refresh_session_id,
                    Some(switched_client),
                )
                .await;
            }
            return Ok(AttachedSwitchCommitOutcome {
                previous_session_id,
                client_name,
                committed_target,
            });
        }

        Err(RmuxError::Server(format!(
            "session {} active window changed during attached-size selection",
            request.session_name
        ))
        .into())
    }
}

fn rollback_switch_runtime(
    state: &mut crate::pane_terminals::HandlerState,
    target: &WindowTarget,
    cause: RmuxError,
) -> RmuxError {
    match state
        .resize_window_terminal_runtime(target.session_name(), target.window_index())
        .map_err(RmuxError::from)
    {
        Ok(_) => cause,
        Err(rollback_error) => RmuxError::Server(format!(
            "failed to roll back switch-client runtime after {cause}: {rollback_error}"
        )),
    }
}
