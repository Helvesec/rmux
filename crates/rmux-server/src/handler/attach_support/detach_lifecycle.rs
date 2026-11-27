use super::{
    attached_client_required, ActiveAttach, AttachControl, AttachedClientControlOutcome,
    LifecycleEvent, Ordering, OverlayFrame, RequestHandler, SessionName,
};

impl RequestHandler {
    pub(in crate::handler) async fn emit_attached_client_detached(
        &self,
        outcome: AttachedClientControlOutcome,
    ) {
        if let Some(event) = outcome.detached_event {
            // Capture belongs to the detach commit; sequencing follows its
            // geometry publication, even if the old session has since retired.
            let queued = super::super::lifecycle_support::sequence_prepared_lifecycle_event(
                &mut *self.state.lock().await,
                event,
            );
            self.emit_prepared(queued).await;
        } else {
            self.emit(rmux_core::LifecycleEvent::ClientDetached {
                session_name: outcome.session_name,
                client_name: Some(outcome.client_name),
            })
            .await;
        }
    }

    pub(in crate::handler) async fn finish_failed_attached_delivery_removal(
        &self,
        removed: ActiveAttach,
        session_name: SessionName,
    ) {
        let outcome = Self::failed_attached_delivery_outcome(removed, session_name);
        self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
            session_name: outcome.session_name,
            client_name: Some(outcome.client_name),
        })
        .await;
    }

    pub(in crate::handler) fn failed_attached_delivery_outcome(
        mut removed: ActiveAttach,
        session_name: SessionName,
    ) -> AttachedClientControlOutcome {
        let overlay = removed.overlay.take();
        let client_name = removed.client_name;
        terminate_overlay_job(overlay);
        AttachedClientControlOutcome {
            detached_event: None,
            session_name,
            client_name,
        }
    }

    pub(super) async fn attach_switch_changes_session(
        &self,
        attach_pid: u32,
        expected_attach_id: Option<u64>,
        next_session_name: Option<&rmux_proto::SessionName>,
        next_session_id: Option<rmux_proto::SessionId>,
        command_name: &str,
    ) -> Result<bool, rmux_proto::RmuxError> {
        let Some(next_session_name) = next_session_name else {
            return Ok(false);
        };
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&attach_pid)
            .ok_or_else(|| attached_client_required(command_name))?;
        if expected_attach_id.is_some_and(|expected| active.id != expected) {
            return Err(attached_client_required(command_name));
        }
        Ok(next_session_name != &active.session_name
            || next_session_id.is_some_and(|session_id| session_id != active.session_id))
    }

    pub(super) async fn close_attached_session<F>(
        &self,
        session_id: rmux_proto::SessionId,
        mut control: F,
    ) where
        F: FnMut() -> AttachControl,
    {
        let mut overlay_jobs = Vec::new();
        let mut removed_pids = Vec::new();
        let mut detached_clients = Vec::new();
        // Clients that remembered the closed session lose their
        // `#{client_last_session}` and get the redraw tmux gives them.
        let mut sessions_remembering_closed = Vec::new();
        let mut state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        for active in active_attach.by_pid.values_mut() {
            if active.last_session_id == Some(session_id) {
                active.last_session = None;
                active.last_session_id = None;
                if active.session_id != session_id
                    && !sessions_remembering_closed.contains(&active.session_name)
                {
                    sessions_remembering_closed.push(active.session_name.clone());
                }
            }
        }
        active_attach.by_pid.retain(|pid, active| {
            if active.session_id != session_id {
                return true;
            }

            overlay_jobs.push(active.overlay.take());
            removed_pids.push(*pid);
            let emit_detached = active.needs_detached_event();
            if emit_detached {
                detached_clients.push((active.session_name.clone(), active.client_name.clone()));
            }
            let _ = active.control_tx.send(control());
            active.closing.store(true, Ordering::SeqCst);
            false
        });
        for pid in removed_pids {
            active_attach.forget_attached_client_windows(pid);
        }
        let focus_effects = super::super::focus_support::prepare_server_focus_effects(
            &mut state,
            &mut active_attach,
        );
        drop(active_attach);
        drop(state);
        self.bump_active_attach_epoch();
        let _ = focus_effects.execute(self).await;
        for overlay in overlay_jobs {
            terminate_overlay_job(overlay);
        }
        for (session_name, client_name) in detached_clients {
            self.emit_without_attached_refresh(LifecycleEvent::ClientDetached {
                session_name,
                client_name: Some(client_name),
            })
            .await;
        }
        for session_name in sessions_remembering_closed {
            self.refresh_attached_session(&session_name).await;
        }
    }
}

pub(super) fn reset_interactive_attach_state_for_session_switch(
    active: &mut ActiveAttach,
) -> Option<super::super::overlay_support::ClientOverlayState> {
    active.mouse.reset_for_session_switch();
    active.prompt = None;
    active.display_panes = None;
    active.display_panes_state_id = active.display_panes_state_id.saturating_add(1);
    active.mode_tree = None;
    active.mode_tree_frame = None;
    active.mode_tree_state_id = active.mode_tree_state_id.saturating_add(1);
    active
        .persistent_overlay_epoch
        .store(active.mode_tree_state_id, Ordering::SeqCst);
    active.overlay_generation = active.overlay_generation.saturating_add(1);
    active.transient_message = None;
    active.transient_terminal_prefix.clear();
    active.overlay_state_id = active.overlay_state_id.saturating_add(1);
    let retired = active.overlay.take();
    if retired.is_some() {
        // The render loop keeps its own "an overlay is on screen" flag and
        // holds back base-pane repaints while it is set. Retiring the overlay
        // here without telling it latches that flag: the client lands on the
        // new session with the old popup still drawn, and — since the pane it
        // switched to may be idle — no later output ever arrives to lift the
        // hold. Every other dismissal path sends this empty frame; a session
        // switch is one too.
        let _ = active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::persistent(
                Vec::new(),
                active.render_generation,
                active.overlay_generation,
            )));
    }
    retired
}

pub(super) fn terminate_overlay_job(
    overlay: Option<super::super::overlay_support::ClientOverlayState>,
) {
    if let Some(super::super::overlay_support::ClientOverlayState::Popup(popup)) = overlay {
        if let Some(job) = popup.job {
            job.terminate();
        }
    }
}
