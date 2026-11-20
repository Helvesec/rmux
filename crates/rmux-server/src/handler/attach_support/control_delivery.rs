use super::{
    attached_client_required, reset_interactive_attach_state_for_session_switch,
    terminate_overlay_job, AttachControl, AttachControlIdentityExpectation,
    AttachedClientControlOutcome, LifecycleEvent, Ordering, RequestHandler,
};
#[cfg(test)]
use super::{pause_after_attach_control_identity_capture, SwitchTargetSelection};

impl RequestHandler {
    pub(in crate::handler) async fn send_attach_control(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation::default(),
        )
        .await
        .map(|outcome| outcome.session_name)
    }

    #[cfg(test)]
    pub(in crate::handler) async fn send_attach_control_for_session_identity(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: rmux_proto::SessionId,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                next_session: Some((next_session_name, expected_session_id)),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
        .map(|outcome| outcome.session_name)
    }

    #[cfg(test)]
    pub(in crate::handler) async fn send_attach_control_for_client_and_session_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: rmux_proto::SessionId,
        target_selection: Option<SwitchTargetSelection>,
    ) -> Result<rmux_proto::SessionName, rmux_proto::RmuxError> {
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            "switch-client",
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                next_session: Some((next_session_name, expected_session_id)),
                target_selection,
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
        .map(|outcome| outcome.session_name)
    }

    pub(in crate::handler) async fn send_attach_control_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        command_name: &str,
    ) -> Result<AttachedClientControlOutcome, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    pub(in crate::handler) async fn send_attach_control_for_client_identity_from_mode_tree(
        &self,
        requester: super::super::mode_tree_support::ModeTreeActionIdentity,
        attach_pid: u32,
        expected_attach_id: u64,
        command: AttachControl,
        command_name: &str,
    ) -> Result<AttachedClientControlOutcome, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                mode_tree_requester: Some(requester),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    pub(in crate::handler) async fn send_attach_control_for_client_current_session_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        expected_current_session_id: rmux_proto::SessionId,
        command: AttachControl,
        command_name: &str,
    ) -> Result<AttachedClientControlOutcome, rmux_proto::RmuxError> {
        if matches!(command, AttachControl::Switch(_)) {
            return Err(rmux_proto::RmuxError::Server(
                "session switch requires a stable target session identity".to_owned(),
            ));
        }
        self.send_attach_control_with_expected_identity(
            attach_pid,
            command,
            command_name,
            AttachControlIdentityExpectation {
                attach_id: Some(expected_attach_id),
                current_session_id: Some(expected_current_session_id),
                ..AttachControlIdentityExpectation::default()
            },
        )
        .await
    }

    async fn send_attach_control_with_expected_identity(
        &self,
        attach_pid: u32,
        command: AttachControl,
        command_name: &str,
        identity: AttachControlIdentityExpectation,
    ) -> Result<AttachedClientControlOutcome, rmux_proto::RmuxError> {
        let expected_attach_id = identity.attach_id;
        let expected_current_session_id = identity.current_session_id;
        let target_selection = identity.target_selection;
        let mode_tree_requester = identity.mode_tree_requester;
        let (next_session_name, expected_next_session_id) = identity
            .next_session
            .map_or((None, None), |(name, session_id)| {
                (Some(name), Some(session_id))
            });
        let is_switch = matches!(command, AttachControl::Switch(_));
        let next_session_id = if let Some(session_name) = next_session_name.as_ref() {
            let state = self.state.lock().await;
            let session_id = state
                .sessions
                .session(session_name)
                .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?
                .id();
            if expected_next_session_id.is_some_and(|expected| expected != session_id) {
                return Err(rmux_proto::RmuxError::SessionNotFound(
                    session_name.to_string(),
                ));
            }
            Some(session_id)
        } else {
            None
        };
        #[cfg(test)]
        pause_after_attach_control_identity_capture(attach_pid).await;
        let switch_changes_session = if is_switch {
            self.attach_switch_changes_session(
                attach_pid,
                expected_attach_id,
                next_session_name.as_ref(),
                next_session_id,
                command_name,
            )
            .await?
        } else {
            false
        };
        let mode_tree_refresh_sessions = if switch_changes_session {
            match expected_attach_id {
                Some(attach_id) => {
                    self.dismiss_mode_tree_for_client_identity(attach_pid, attach_id)
                        .await?
                }
                None => self.dismiss_mode_tree(attach_pid).await?,
            }
        } else {
            Vec::new()
        };
        let clear_prompt = matches!(
            command,
            AttachControl::Switch(_)
                | AttachControl::Detach
                | AttachControl::Exited
                | AttachControl::DetachKill
                | AttachControl::DetachExecShellCommand(_)
        );
        // Keep the model generation stable until the attached client has been
        // updated. Otherwise a kill/recreate of the same session name between
        // target construction and this assignment can bind the client to a
        // stale target while recording the replacement session's name.
        let mut state = self.state.lock().await;
        if let (Some(session_name), Some(expected_session_id)) =
            (next_session_name.as_ref(), next_session_id)
        {
            let current_session_id = state
                .sessions
                .session(session_name)
                .map(|session| session.id());
            if current_session_id != Some(expected_session_id) {
                return Err(rmux_proto::RmuxError::SessionNotFound(
                    session_name.to_string(),
                ));
            }
        }
        let mut active_attach = self.active_attach.lock().await;
        if mode_tree_requester
            .is_some_and(|requester| !requester.matches_active(&state, &active_attach))
        {
            return Err(attached_client_required(command_name));
        }
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Err(attached_client_required(command_name));
        };
        if expected_attach_id.is_some_and(|expected| active.id != expected) {
            return Err(attached_client_required(command_name));
        }
        if active.closing.load(Ordering::SeqCst) {
            return Err(attached_client_required(command_name));
        }
        if expected_current_session_id.is_some_and(|expected| active.session_id != expected) {
            return Err(attached_client_required(command_name));
        }
        if let Some(selection) = target_selection.as_ref() {
            if !is_switch {
                return Err(rmux_proto::RmuxError::Server(
                    "target selection requires a session switch".to_owned(),
                ));
            }
            let session_name = next_session_name
                .as_ref()
                .expect("a switch target selection carries a session");
            let session_id = next_session_id
                .expect("a switch target selection carries a stable session identity");
            selection.validate_for_session_identity(&state, session_name, session_id)?;
        }
        let previous_session_name = active.session_name.clone();
        let client_name = active.client_name.clone();
        let mut overlay_to_terminate = None;

        let switches_session_identity = next_session_name.as_ref().is_some_and(|session_name| {
            session_name != &active.session_name
                || next_session_id.is_some_and(|session_id| session_id != active.session_id)
        });
        if is_switch && switches_session_identity {
            overlay_to_terminate = reset_interactive_attach_state_for_session_switch(active);
        }
        let closing_control = matches!(
            command,
            AttachControl::Detach
                | AttachControl::Exited
                | AttachControl::DetachKill
                | AttachControl::DetachExecShellCommand(_)
        );
        let render_stream_switch_refresh = active.render_stream
            && matches!(
                &command,
                AttachControl::Switch(target) if target.is_coalescible_render_refresh()
            );
        if render_stream_switch_refresh {
            if let Some(session_name) = next_session_name {
                if switches_session_identity {
                    active.last_session = Some(active.session_name.clone());
                    active.last_session_id = Some(active.session_id);
                }
                active.session_name = session_name;
                active.session_id = next_session_id
                    .expect("a session switch target must carry its stable identity");
            }
            if !active.render_refresh_pending {
                active.render_refresh_pending = true;
                if let Err(error) = active.control_tx.send(AttachControl::Refresh) {
                    if error.is_full() {
                        active.closing.store(true, Ordering::SeqCst);
                    }
                    let removed = active_attach
                        .remove_attached_client(attach_pid)
                        .expect("failed attached refresh removes the active identity");
                    self.bump_active_attach_epoch();
                    let focus_effects = super::super::focus_support::prepare_server_focus_effects(
                        &mut state,
                        &mut active_attach,
                    );
                    let error = if error.is_full() {
                        rmux_proto::RmuxError::Server(
                            "attached client is not draining updates".to_owned(),
                        )
                    } else {
                        attached_client_required(command_name)
                    };
                    drop(active_attach);
                    drop(state);
                    let _ = focus_effects.execute(self).await;
                    terminate_overlay_job(overlay_to_terminate);
                    self.finish_failed_attached_delivery_removal(removed, previous_session_name)
                        .await;
                    return Err(error);
                }
            }
            if let Some(selection) = target_selection.as_ref() {
                selection
                    .apply_to_state(&mut state)
                    .expect("prevalidated switch selection remains applicable while locked");
            }
            if expected_attach_id.is_some() {
                state
                    .sessions
                    .touch_attached(&active.session_name)
                    .expect("switch target stayed locked across the attached client update");
            }
            drop(active_attach);
            drop(state);
            if clear_prompt {
                match expected_attach_id {
                    Some(attach_id) => {
                        self.clear_prompt_for_attach_identity(attach_pid, attach_id)
                            .await;
                    }
                    None => self.clear_prompt_for_attach(attach_pid).await,
                }
            }
            for session_name in mode_tree_refresh_sessions {
                self.refresh_attached_session(&session_name).await;
            }
            terminate_overlay_job(overlay_to_terminate);
            return Ok(AttachedClientControlOutcome {
                detached_event: None,
                session_name: previous_session_name,
                client_name,
            });
        }
        if is_switch {
            active.render_generation = active.render_generation.saturating_add(1);
        }
        if let Err(error) = active.control_tx.send(command) {
            if error.is_full() {
                active.closing.store(true, Ordering::SeqCst);
            }
            let removed = active_attach
                .remove_attached_client(attach_pid)
                .expect("failed attached control removes the active identity");
            self.bump_active_attach_epoch();
            let focus_effects = super::super::focus_support::prepare_server_focus_effects(
                &mut state,
                &mut active_attach,
            );
            let error = if error.is_full() {
                rmux_proto::RmuxError::Server("attached client is not draining updates".to_owned())
            } else {
                attached_client_required(command_name)
            };
            drop(active_attach);
            drop(state);
            let _ = focus_effects.execute(self).await;
            terminate_overlay_job(overlay_to_terminate);
            self.finish_failed_attached_delivery_removal(removed, previous_session_name)
                .await;
            return Err(error);
        }
        if let Some(selection) = target_selection.as_ref() {
            selection
                .apply_to_state(&mut state)
                .expect("prevalidated switch selection remains applicable while locked");
        }
        if closing_control {
            active.detach_event_owned = true;
            active.closing.store(true, Ordering::SeqCst);
        }
        if let Some(session_name) = next_session_name {
            if switches_session_identity {
                active.last_session = Some(active.session_name.clone());
                active.last_session_id = Some(active.session_id);
            }
            active.session_name = session_name;
            active.session_id =
                next_session_id.expect("a session switch target must carry its stable identity");
        }
        if is_switch && expected_attach_id.is_some() {
            state
                .sessions
                .touch_attached(&active.session_name)
                .expect("switch target stayed locked across the attached client update");
        }
        let detached_event = closing_control.then(|| {
            super::super::lifecycle_support::prepare_unsequenced_lifecycle_event(
                &mut state,
                &LifecycleEvent::ClientDetached {
                    session_name: previous_session_name.clone(),
                    client_name: Some(client_name.clone()),
                },
            )
        });
        let focus_effects = closing_control.then(|| {
            super::super::focus_support::prepare_server_focus_effects(
                &mut state,
                &mut active_attach,
            )
        });
        drop(active_attach);
        drop(state);

        if let Some(focus_effects) = focus_effects {
            let _ = focus_effects.execute(self).await;
        }

        if clear_prompt {
            match expected_attach_id {
                Some(attach_id) => {
                    self.clear_prompt_for_attach_identity(attach_pid, attach_id)
                        .await;
                }
                None => self.clear_prompt_for_attach(attach_pid).await,
            }
        }
        for session_name in mode_tree_refresh_sessions {
            self.refresh_attached_session(&session_name).await;
        }
        terminate_overlay_job(overlay_to_terminate);

        Ok(AttachedClientControlOutcome {
            detached_event,
            session_name: previous_session_name,
            client_name,
        })
    }
}
