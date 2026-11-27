use super::*;

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_control_switch_post_commit_pause(
        &self,
        requester_pid: u32,
    ) -> Arc<ControlSwitchPostCommitPause> {
        let pause = Arc::new(ControlSwitchPostCommitPause::default());
        let mut installed = CONTROL_SWITCH_POST_COMMIT_PAUSE
            .lock()
            .expect("control switch post-commit pause lock");
        installed.retain(|(paused_pid, _)| *paused_pid != requester_pid);
        installed.push((requester_pid, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_after_control_switch_commit(&self, requester_pid: u32) {
        let pause = {
            let mut installed = CONTROL_SWITCH_POST_COMMIT_PAUSE
                .lock()
                .expect("control switch post-commit pause lock");
            installed
                .iter()
                .position(|(paused_pid, _)| *paused_pid == requester_pid)
                .map(|position| installed.swap_remove(position).1)
        };
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    #[cfg(test)]
    pub(in crate::handler) async fn set_control_session(
        &self,
        requester_pid: u32,
        next_session_name: Option<rmux_proto::SessionName>,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            next_session_name,
            None,
            None,
            ControlSessionUpdate::existing(None, None),
        )
        .await
    }

    #[cfg(test)]
    pub(in crate::handler) async fn set_control_session_identity(
        &self,
        requester_pid: u32,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            None,
            ControlSessionUpdate::existing(None, None),
        )
        .await
    }

    pub(in crate::handler) async fn attach_existing_control_session_for_client_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            Some(expected_control_id),
            ControlSessionUpdate::attached_existing(),
        )
        .await
    }

    pub(in crate::handler) async fn set_created_control_session_for_client_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        next_session_name: rmux_proto::SessionName,
        expected_session_id: SessionId,
    ) -> Result<Option<rmux_proto::SessionName>, rmux_proto::RmuxError> {
        self.set_control_session_with_expected_identity(
            requester_pid,
            Some(next_session_name),
            Some(expected_session_id),
            Some(expected_control_id),
            ControlSessionUpdate::created(),
        )
        .await
    }

    pub(in crate::handler) async fn prepare_control_session_switch_after_destroy(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        expected_current_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Option<(rmux_proto::SessionName, QueuedLifecycleEvent)> {
        let outcome = self
            .switch_control_session_after_destroy_inner(
                requester_pid,
                expected_control_id,
                expected_current_session_id,
                target_session_id,
            )
            .await?;
        #[cfg(test)]
        self.pause_after_control_switch_commit(requester_pid).await;
        Some((outcome.target_session_name, outcome.client_session_changed))
    }

    async fn switch_control_session_after_destroy_inner(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        expected_current_session_id: SessionId,
        target_session_id: SessionId,
    ) -> Option<DestroyControlSwitchCommitOutcome> {
        let mut state = self.state.lock().await;
        let target_session_name = state
            .sessions
            .session_by_id(target_session_id)?
            .name()
            .clone();
        let pane_sequences = current_pane_output_sequences(&state, &target_session_name).ok()?;
        let size_sequence = self.next_client_size_sequence();
        let mut active_control = self.active_control.lock().await;
        let active = active_control
            .by_pid
            .get_mut(&requester_pid)
            .filter(|active| {
                active.id == expected_control_id
                    && active.session_id == Some(expected_current_session_id)
                    && !active.closing.load(Ordering::SeqCst)
            })?;
        let (_, delivered) = update_control_session(
            active,
            Some(target_session_name.clone()),
            Some(target_session_id),
            Some(pane_sequences),
        );
        if !delivered {
            return None;
        }
        active.size_sequence = size_sequence;
        state
            .sessions
            .touch_attached(&target_session_name)
            .expect("stable destroy-switch target remains locked");
        let mut client_session_changed = super::super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::ClientSessionChanged {
                session_name: target_session_name.clone(),
                client_name: Some(control_client_name(requester_pid)),
            },
        );
        client_session_changed.control_session_identity = Some(target_session_id);
        Some(DestroyControlSwitchCommitOutcome {
            target_session_name,
            client_session_changed,
        })
    }

    pub(in crate::handler) async fn attach_control_session_for_queue(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
    ) -> Result<bool, rmux_proto::RmuxError> {
        self.attach_control_session_for_queue_with_output_start(
            identity,
            session_name,
            expected_session_id,
            false,
        )
        .await
    }

    pub(in crate::handler) async fn attach_created_control_session_for_queue(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
    ) -> Result<bool, rmux_proto::RmuxError> {
        self.attach_control_session_for_queue_with_output_start(
            identity,
            session_name,
            expected_session_id,
            true,
        )
        .await
    }

    async fn attach_control_session_for_queue_with_output_start(
        &self,
        identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<SessionId>,
        replay_from_oldest: bool,
    ) -> Result<bool, rmux_proto::RmuxError> {
        let mut state = self.state.lock().await;
        let mut active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )?;

        let session_id = state
            .sessions
            .session(session_name)
            .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?
            .id();
        if expected_session_id.is_some_and(|expected| expected != session_id) {
            return Err(rmux_proto::RmuxError::SessionNotFound(
                session_name.to_string(),
            ));
        }
        if active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.session_name.as_ref() == Some(session_name)
                    && active.session_id == Some(session_id)
            })
        {
            return Ok(false);
        }

        let pane_sequences = if replay_from_oldest {
            None
        } else {
            Some(current_pane_output_sequences(&state, session_name)?)
        };
        let size_sequence = self.next_client_size_sequence();

        let delivered = {
            let active = active_control
                .by_pid
                .get_mut(&identity.requester_pid())
                .expect("validated control client remains registered while locked");
            update_control_session(
                active,
                Some(session_name.clone()),
                Some(session_id),
                pane_sequences,
            )
            .1
        };
        if !delivered {
            return Err(attached_client_required("control session"));
        }
        active_control
            .by_pid
            .get_mut(&identity.requester_pid())
            .expect("delivered control client remains registered while locked")
            .size_sequence = size_sequence;
        state
            .sessions
            .touch_attached(session_name)
            .expect("validated session remains present while locked");
        Ok(true)
    }
}
