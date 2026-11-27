use super::*;

impl RequestHandler {
    pub(in crate::handler) async fn attached_count(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> usize {
        let attach_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(session_name)
        };
        let control_count = {
            let active_control = self.active_control.lock().await;
            active_control.attached_count(session_name)
        };

        attach_count.saturating_add(control_count)
    }

    pub(in crate::handler) async fn attached_count_for_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
    ) -> usize {
        let attach_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .values()
                .filter(|active| {
                    &active.session_name == session_name
                        && active.session_id == session_id
                        && !active.suspended
                })
                .count()
        };
        let control_count = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .values()
                .filter(|active| {
                    active.session_name.as_ref() == Some(session_name)
                        && active.session_id == Some(session_id)
                })
                .count()
        };

        attach_count.saturating_add(control_count)
    }

    pub(in crate::handler) async fn attached_count_after_switch(
        &self,
        session_name: &rmux_proto::SessionName,
        client: ManagedClient,
    ) -> usize {
        let attached_count = self.attached_count(session_name).await;

        match client {
            ManagedClient::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let active_attach = self.active_attach.lock().await;
                if active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
                    active.id == attach_id && &active.session_name == session_name
                }) {
                    attached_count
                } else {
                    attached_count.saturating_add(1)
                }
            }
            ManagedClient::Control(identity) => {
                let active_control = self.active_control.lock().await;
                if active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| active.id == identity.control_id())
                    .and_then(|active| active.session_name.as_ref())
                    .is_some_and(|active| active == session_name)
                {
                    attached_count
                } else {
                    attached_count.saturating_add(1)
                }
            }
        }
    }

    pub(in crate::handler) async fn rename_control_session(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        new_name: &rmux_proto::SessionName,
    ) {
        let mut active_control = self.active_control.lock().await;
        for active in active_control.by_pid.values_mut() {
            if active.session_name.as_ref() == Some(session_name)
                && active.session_id == Some(session_id)
            {
                active.session_name = Some(new_name.clone());
                let _ = try_send_control_event(
                    active,
                    ControlServerEvent::SessionChanged(Some(new_name.clone())),
                );
            }
            if active.last_session.as_ref() == Some(session_name)
                && active.last_session_id == Some(session_id)
            {
                active.last_session = Some(new_name.clone());
            }
        }
    }

    pub(in crate::handler) async fn current_session_candidate(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            Self::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                identity.control_id(),
            )
            .ok()?;
            return active_control
                .by_pid
                .get(&requester_pid)
                .and_then(|active| active.session_name.clone());
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(candidate) = active_attach.current_session_candidate(requester_pid) {
                return Some(candidate);
            }
        }

        let candidate = {
            let active_control = self.active_control.lock().await;
            active_control.current_session_candidate(requester_pid)
        }?;
        let state = self.state.lock().await;
        state
            .sessions
            .session(&candidate.0)
            .is_some_and(|session| session.id() == candidate.1)
            .then_some(candidate.0)
    }

    pub(in crate::handler) async fn validate_control_queue_session_identity(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
    ) -> Result<(), rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            requester_pid,
            expected_control_id,
        )
    }

    pub(in crate::handler) fn validate_control_queue_identity_locked(
        state: &HandlerState,
        active_control: &ActiveControlState,
        requester_pid: u32,
        expected_control_id: u64,
    ) -> Result<(), rmux_proto::RmuxError> {
        let Some(active) = active_control.by_pid.get(&requester_pid) else {
            return Err(attached_client_required("control command"));
        };
        if active.id != expected_control_id {
            return Err(attached_client_required("control command"));
        }
        if active.closing.load(Ordering::SeqCst) {
            return Err(rmux_proto::RmuxError::Server(
                "control client is closing".to_owned(),
            ));
        }
        match (active.session_name.as_ref(), active.session_id) {
            (None, None) => Ok(()),
            (Some(session_name), Some(session_id))
                if state
                    .sessions
                    .session(session_name)
                    .is_some_and(|session| session.id() == session_id) =>
            {
                Ok(())
            }
            (Some(session_name), _) => Err(rmux_proto::RmuxError::SessionNotFound(
                session_name.to_string(),
            )),
            (None, Some(_)) => Err(rmux_proto::RmuxError::Server(
                "control client has an invalid session identity".to_owned(),
            )),
        }
    }

    #[cfg(test)]
    pub(in crate::handler) async fn control_queue_client_id(
        &self,
        requester_pid: u32,
    ) -> Result<u64, rmux_proto::RmuxError> {
        let active_control = self.active_control.lock().await;
        let active = active_control
            .by_pid
            .get(&requester_pid)
            .ok_or_else(|| attached_client_required("control command"))?;
        if active.closing.load(Ordering::SeqCst) {
            return Err(rmux_proto::RmuxError::Server(
                "control client is closing".to_owned(),
            ));
        }
        Ok(active.id)
    }

    pub(in crate::handler) async fn resolve_managed_client(
        &self,
        requester_pid: u32,
        command_name: &str,
    ) -> Result<ManagedClient, rmux_proto::RmuxError> {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            Self::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                identity.control_id(),
            )?;
            return Ok(ManagedClient::Control(identity));
        }

        {
            let active_attach = self.active_attach.lock().await;
            if let Some(active) = active_attach
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(Ordering::SeqCst))
            {
                return Ok(ManagedClient::Attach {
                    pid: requester_pid,
                    attach_id: active.id,
                });
            }
        }
        {
            let active_control = self.active_control.lock().await;
            if let Some(active) = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(Ordering::SeqCst))
            {
                return Ok(ManagedClient::Control(ControlClientIdentity::new(
                    requester_pid,
                    active.id,
                )));
            }
        }

        let attach_candidates = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                .map(|(&pid, active)| (pid, active.id))
                .collect::<Vec<_>>()
        };
        let control_candidates = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                .map(|(&pid, active)| ControlClientIdentity::new(pid, active.id))
                .collect::<Vec<_>>()
        };

        match attach_candidates.len() + control_candidates.len() {
            0 if command_name == "show-messages" => Err(rmux_proto::RmuxError::Message(
                "no current client".to_owned(),
            )),
            0 => Err(attached_client_required(command_name)),
            1 => {
                if let Some((pid, attach_id)) = attach_candidates.first().copied() {
                    Ok(ManagedClient::Attach { pid, attach_id })
                } else {
                    Ok(ManagedClient::Control(
                        control_candidates
                            .first()
                            .copied()
                            .expect("single control candidate"),
                    ))
                }
            }
            _ => Err(ambiguous_attached_client(command_name)),
        }
    }

    pub(crate) async fn control_session_name(
        &self,
        requester_pid: u32,
    ) -> Option<rmux_proto::SessionName> {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                expected_control_id.is_none_or(|expected| {
                    active.id == expected && !active.closing.load(Ordering::SeqCst)
                })
            })
            .and_then(|active| active.session_name.clone())
    }

    pub(in crate::handler) async fn control_queue_can_write(
        &self,
        identity: ControlClientIdentity,
    ) -> bool {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        if Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )
        .is_err()
        {
            return false;
        }
        active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| active.can_write)
    }

    pub(crate) async fn is_control_client(&self, requester_pid: u32) -> bool {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .is_some_and(|active| {
                expected_control_id.is_none_or(|expected| active.id == expected)
                    && !active.closing.load(Ordering::SeqCst)
            })
    }

    /// Whether the command running on this control queue comes from a client
    /// tmux would mark `CLIENT_READONLY` (`attach-session -r`, `-f read-only`).
    pub(in crate::handler) async fn control_queue_is_read_only(&self, requester_pid: u32) -> bool {
        current_control_queue_identity(requester_pid).is_some()
            && self
                .control_client_flags(requester_pid)
                .await
                .is_some_and(|flags| flags.read_only)
    }

    pub(crate) async fn control_client_flags(
        &self,
        requester_pid: u32,
    ) -> Option<ControlClientFlags> {
        let expected_control_id =
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id);
        let active_control = self.active_control.lock().await;
        active_control
            .by_pid
            .get(&requester_pid)
            .filter(|active| {
                expected_control_id.is_none_or(|expected| {
                    active.id == expected && !active.closing.load(Ordering::SeqCst)
                })
            })
            .map(|active| active.flags)
    }
}
