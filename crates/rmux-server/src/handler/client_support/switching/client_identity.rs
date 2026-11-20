use super::*;

impl RequestHandler {
    pub(super) async fn current_managed_session_identity(
        &self,
        client: SwitchManagedClientIdentity,
    ) -> Result<SwitchSessionIdentity, RmuxError> {
        match client {
            SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } => {
                let state = self.state.lock().await;
                let active_attach = self.active_attach.lock().await;
                let active = active_attach
                    .by_pid
                    .get(&attach_pid)
                    .filter(|active| active.id == attach_id)
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                if state
                    .sessions
                    .session(&active.session_name)
                    .is_none_or(|session| session.id() != active.session_id)
                {
                    return Err(session_not_found(&active.session_name));
                }
                Ok(SwitchSessionIdentity {
                    session_name: active.session_name.clone(),
                    session_id: active.session_id,
                })
            }
            SwitchManagedClientIdentity::Control {
                pid: control_pid,
                control_id,
            } => {
                let state = self.state.lock().await;
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&control_pid)
                    .filter(|active| {
                        active.id == control_id && !active.closing.load(Ordering::SeqCst)
                    })
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                let session_name = active
                    .session_name
                    .clone()
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                let session_id = active
                    .session_id
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                if state
                    .sessions
                    .session(&session_name)
                    .is_none_or(|session| session.id() != session_id)
                {
                    return Err(session_not_found(&session_name));
                }
                Ok(SwitchSessionIdentity {
                    session_name,
                    session_id,
                })
            }
        }
    }

    pub(super) async fn resolve_switch_managed_client_identity(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
    ) -> Result<SwitchManagedClientIdentity, RmuxError> {
        let target_client = target_client.map(normalize_target_client);
        if target_client.is_none() || target_client == Some("=") {
            if let Some(identity) = current_control_queue_identity(requester_pid) {
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&requester_pid)
                    .filter(|active| {
                        active.id == identity.control_id() && !active.closing.load(Ordering::SeqCst)
                    })
                    .ok_or_else(|| attached_client_required("switch-client"))?;
                return Ok(SwitchManagedClientIdentity::Control {
                    pid: requester_pid,
                    control_id: active.id,
                });
            }
            {
                let active_attach = self.active_attach.lock().await;
                if let Some(active) = active_attach.by_pid.get(&requester_pid) {
                    return Ok(SwitchManagedClientIdentity::Attach {
                        pid: requester_pid,
                        attach_id: active.id,
                    });
                }
            }
            {
                let active_control = self.active_control.lock().await;
                if let Some(active) = active_control.by_pid.get(&requester_pid) {
                    if active.closing.load(Ordering::SeqCst) {
                        return Err(attached_client_required("switch-client"));
                    }
                    return Ok(SwitchManagedClientIdentity::Control {
                        pid: requester_pid,
                        control_id: active.id,
                    });
                }
            }

            let attach_candidates = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .iter()
                    .map(|(&pid, active)| SwitchManagedClientIdentity::Attach {
                        pid,
                        attach_id: active.id,
                    })
                    .collect::<Vec<_>>()
            };
            let control_candidates = {
                let active_control = self.active_control.lock().await;
                active_control
                    .by_pid
                    .iter()
                    .map(|(&pid, active)| SwitchManagedClientIdentity::Control {
                        pid,
                        control_id: active.id,
                    })
                    .collect::<Vec<_>>()
            };
            let candidate = match attach_candidates.len() + control_candidates.len() {
                0 => Err(attached_client_required("switch-client")),
                1 => Ok(attach_candidates
                    .into_iter()
                    .chain(control_candidates)
                    .next()
                    .expect("one switch-client candidate exists")),
                _ => Err(ambiguous_attached_client("switch-client")),
            }?;
            self.validate_switch_managed_client_identity(candidate)
                .await?;
            return Ok(candidate);
        }

        let target_client = target_client.expect("target client was normalized above");
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            self.validate_control_queue_session_identity(requester_pid, identity.control_id())
                .await?;
        }
        {
            let active_attach = self.active_attach.lock().await;
            let matched = if let Ok(pid) = target_client.parse::<u32>() {
                active_attach.by_pid.get(&pid).map(|active| (pid, active))
            } else {
                active_attach.by_pid.iter().find_map(|(&pid, active)| {
                    attached_client_matches_target(&active.client_name, target_client)
                        .then_some((pid, active))
                })
            };
            if let Some((pid, active)) = matched {
                return Ok(SwitchManagedClientIdentity::Attach {
                    pid,
                    attach_id: active.id,
                });
            }
        }
        if let Some(pid) = control_client_target_pid(target_client) {
            let active_control = self.active_control.lock().await;
            if let Some(active) = active_control.by_pid.get(&pid) {
                if active.closing.load(Ordering::SeqCst) {
                    return Err(attached_client_required("switch-client"));
                }
                return Ok(SwitchManagedClientIdentity::Control {
                    pid,
                    control_id: active.id,
                });
            }
        }

        Err(RmuxError::Server(format!(
            "can't find client: {target_client}"
        )))
    }

    pub(super) async fn validate_switch_managed_client_identity(
        &self,
        client: SwitchManagedClientIdentity,
    ) -> Result<(), RmuxError> {
        let current = match client {
            SwitchManagedClientIdentity::Attach { pid, attach_id } => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&pid)
                    .is_some_and(|active| active.id == attach_id)
            }
            SwitchManagedClientIdentity::Control { pid, control_id } => {
                let active_control = self.active_control.lock().await;
                active_control.by_pid.get(&pid).is_some_and(|active| {
                    active.id == control_id && !active.closing.load(Ordering::SeqCst)
                })
            }
        };
        current
            .then_some(())
            .ok_or_else(|| attached_client_required("switch-client"))
    }

    pub(super) async fn with_switch_client_state<R>(
        &self,
        client: Option<SwitchManagedClientIdentity>,
        action: impl FnOnce(&mut HandlerState) -> Result<R, RmuxError>,
    ) -> Result<R, RmuxError> {
        let mut state = self.state.lock().await;
        match client {
            None => action(&mut state),
            Some(SwitchManagedClientIdentity::Attach { pid, attach_id }) => {
                let active_attach = self.active_attach.lock().await;
                if active_attach
                    .by_pid
                    .get(&pid)
                    .is_none_or(|active| active.id != attach_id)
                {
                    return Err(attached_client_required("switch-client"));
                }
                let result = action(&mut state);
                drop(active_attach);
                result
            }
            Some(SwitchManagedClientIdentity::Control { pid, control_id }) => {
                let active_control = self.active_control.lock().await;
                if active_control.by_pid.get(&pid).is_none_or(|active| {
                    active.id != control_id || active.closing.load(Ordering::SeqCst)
                }) {
                    return Err(attached_client_required("switch-client"));
                }
                let result = action(&mut state);
                drop(active_control);
                result
            }
        }
    }

    pub(super) async fn validate_switch_destination(
        &self,
        client: SwitchManagedClientIdentity,
        session_name: &SessionName,
        session_id: SessionId,
        target_selection: Option<&SwitchTargetSelection>,
    ) -> Result<(), RmuxError> {
        self.with_switch_client_state(Some(client), |state| {
            if state
                .sessions
                .session(session_name)
                .is_none_or(|session| session.id() != session_id)
            {
                return Err(session_not_found(session_name));
            }
            if let Some(selection) = target_selection {
                selection.validate_for_session_identity(state, session_name, session_id)?;
            }
            Ok(())
        })
        .await
    }
}
