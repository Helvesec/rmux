use rmux_proto::{
    ErrorResponse, LockClientResponse, LockServerResponse, LockSessionResponse, OptionName,
    Response, RmuxError, SessionId, SessionName,
};

use super::{
    attach_support::ActiveAttachIdentity, attached_client_matches_target, normalize_target_client,
    RequestHandler,
};
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;
use crate::terminal::TerminalProfile;

#[cfg(test)]
#[path = "handler_lock/identity_test_pause.rs"]
mod identity_test_pause;
#[cfg(test)]
pub(in crate::handler) use identity_test_pause::{
    install_lock_identity_pause, pause_after_lock_identity_capture, LockIdentityPausePoint,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockAttachTarget {
    identity: ActiveAttachIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockSessionPolicy {
    CurrentAttachSession,
    ExactSession(SessionId),
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_lock_server(&self) -> Response {
        let attach_targets = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    !active.suspended && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|(pid, active)| LockAttachTarget {
                    identity: active.identity(*pid),
                })
                .collect::<Vec<_>>()
        };

        #[cfg(test)]
        for target in &attach_targets {
            pause_after_lock_identity_capture(LockIdentityPausePoint::ServerClient(
                target.identity.attach_pid(),
            ))
            .await;
        }

        let mut sessions = Vec::new();
        for target in attach_targets {
            match self
                .lock_attached_client(target, LockSessionPolicy::CurrentAttachSession)
                .await
            {
                Ok(Some(session)) => sessions.push(session),
                Ok(None) => {}
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        }
        sessions.sort_by_key(|(session_name, session_id)| {
            (session_name.to_string(), session_id.as_u32())
        });
        sessions.dedup();
        for (session_name, session_id) in sessions {
            self.refresh_attached_session_for_session_identity(&session_name, session_id)
                .await;
        }
        Response::LockServer(LockServerResponse)
    }

    pub(in crate::handler) async fn handle_lock_session(
        &self,
        request: rmux_proto::LockSessionRequest,
    ) -> Response {
        let session_id = {
            let state = self.state.lock().await;
            if let Err(error) = super::require_expected_session_identity(&state, &request.target) {
                return Response::Error(ErrorResponse { error });
            }
            let Some(session) = state.sessions.session(&request.target) else {
                return Response::Error(ErrorResponse {
                    error: session_not_found(&request.target),
                });
            };
            session.id()
        };

        #[cfg(test)]
        pause_after_lock_identity_capture(LockIdentityPausePoint::Session(request.target.clone()))
            .await;

        let attach_targets = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    active.session_id == session_id
                        && !active.suspended
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|(pid, active)| LockAttachTarget {
                    identity: active.identity(*pid),
                })
                .collect::<Vec<_>>()
        };

        #[cfg(test)]
        pause_after_lock_identity_capture(LockIdentityPausePoint::SessionClients(
            request.target.clone(),
        ))
        .await;

        for attach_target in attach_targets {
            if let Err(error) = self
                .lock_attached_client(attach_target, LockSessionPolicy::ExactSession(session_id))
                .await
            {
                return Response::Error(ErrorResponse { error });
            }
        }
        let current_session_name = {
            let state = self.state.lock().await;
            state
                .sessions
                .session_by_id(session_id)
                .map(|session| session.name().clone())
        };
        if let Some(current_session_name) = current_session_name {
            self.refresh_attached_session_for_session_identity(&current_session_name, session_id)
                .await;
        }
        Response::LockSession(LockSessionResponse {
            target: request.target,
        })
    }

    pub(in crate::handler) async fn handle_lock_client(
        &self,
        requester_pid: u32,
        request: rmux_proto::LockClientRequest,
    ) -> Response {
        let attach_target = match self
            .resolve_lock_client_target(requester_pid, &request.target_client)
            .await
        {
            Ok(target) => target,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        #[cfg(test)]
        pause_after_lock_identity_capture(LockIdentityPausePoint::Client(
            attach_target.identity.attach_pid(),
        ))
        .await;

        let session = match self
            .lock_attached_client(attach_target, LockSessionPolicy::CurrentAttachSession)
            .await
        {
            Ok(Some(session)) => Some(session),
            Ok(None) => None,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        if let Some((session_name, session_id)) = session {
            self.refresh_attached_session_for_session_identity(&session_name, session_id)
                .await;
        }
        Response::LockClient(LockClientResponse {
            target_client: request.target_client,
        })
    }

    async fn resolve_lock_client_target(
        &self,
        requester_pid: u32,
        target_client: &str,
    ) -> Result<LockAttachTarget, RmuxError> {
        let target_client = normalize_target_client(target_client);
        let active_attach = self.active_attach.lock().await;
        if target_client == "=" {
            let attach_pid =
                active_attach.resolve_attached_client_pid(requester_pid, "lock-client")?;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .expect("resolved attached client must remain present under the same lock");
            return Ok(LockAttachTarget {
                identity: active.identity(attach_pid),
            });
        }

        if let Ok(pid) = target_client.parse::<u32>() {
            if let Some(active) = active_attach.by_pid.get(&pid) {
                return Ok(LockAttachTarget {
                    identity: active.identity(pid),
                });
            }
            return Err(RmuxError::Server(format!(
                "lock-client client {pid} is not attached"
            )));
        }

        let attach_targets = active_attach
            .by_pid
            .iter()
            .map(|(pid, active)| LockAttachTarget {
                identity: active.identity(*pid),
            })
            .collect::<Vec<_>>();
        drop(active_attach);

        attach_targets
            .into_iter()
            .find(|target| {
                attached_client_matches_target(target.identity.attach_pid(), target_client)
            })
            .ok_or_else(|| RmuxError::Server(format!("can't find client: {target_client}")))
    }

    async fn lock_attached_client(
        &self,
        target: LockAttachTarget,
        session_policy: LockSessionPolicy,
    ) -> Result<Option<(SessionName, SessionId)>, RmuxError> {
        // Session mutation and attach registration take these locks in this
        // order. Resolve the session selected by the caller's policy and
        // suspend the exact registration under both guards so rename, switch,
        // recreation, and PID reuse are linearizable at one commit point.
        let state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let attach_pid = target.identity.attach_pid();
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid).filter(|active| {
            target.identity.matches_active(active)
                && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
        }) else {
            return Ok(None);
        };
        if active.suspended {
            return Ok(None);
        }
        let session_id = match session_policy {
            LockSessionPolicy::CurrentAttachSession => active.session_id,
            LockSessionPolicy::ExactSession(session_id) if active.session_id == session_id => {
                session_id
            }
            LockSessionPolicy::ExactSession(_) => return Ok(None),
        };
        let Some(session_name) = state
            .sessions
            .session_by_id(session_id)
            .map(|session| session.name().clone())
        else {
            return Ok(None);
        };
        if active.session_name != session_name {
            return Ok(None);
        }
        let command = state
            .options
            .resolve(Some(&session_name), OptionName::LockCommand)
            .or_else(|| state.options.resolve(None, OptionName::LockCommand))
            .map(str::to_owned)
            .unwrap_or_default();
        if command.is_empty() {
            return Ok(None);
        }
        let command = TerminalProfile::for_run_shell(
            &state.environment,
            &state.options,
            Some(&session_name),
            Some(session_id.as_u32()),
            &self.socket_path(),
            !self.config_loading_active(),
            None,
        )?
        .attach_shell_command(command);
        active.suspended = true;
        if active
            .control_tx
            .send(AttachControl::LockShellCommand(command))
            .is_err()
        {
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            return Ok(None);
        }
        Ok(Some((session_name, session_id)))
    }
}
