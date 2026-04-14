use rmux_proto::{
    ErrorResponse, LockClientResponse, LockServerResponse, LockSessionResponse, OptionName,
    Response, RmuxError,
};

use super::{attached_client_matches_target, normalize_target_client, RequestHandler};
use crate::pane_io::AttachControl;
use crate::pane_terminals::session_not_found;

impl RequestHandler {
    pub(in crate::handler) async fn handle_lock_server(&self) -> Response {
        let attach_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| (!active.suspended).then_some(*pid))
                .collect::<Vec<_>>()
        };
        let mut sessions = Vec::new();
        for attach_pid in attach_pids {
            match self.lock_attached_client(attach_pid).await {
                Ok(Some(session_name)) => sessions.push(session_name),
                Ok(None) => {}
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        }
        sessions.sort_by_key(|session_name| session_name.to_string());
        sessions.dedup();
        for session_name in sessions {
            self.refresh_attached_session(&session_name).await;
        }
        Response::LockServer(LockServerResponse)
    }

    pub(in crate::handler) async fn handle_lock_session(
        &self,
        request: rmux_proto::LockSessionRequest,
    ) -> Response {
        {
            let state = self.state.lock().await;
            if state.sessions.session(&request.target).is_none() {
                return Response::Error(ErrorResponse {
                    error: session_not_found(&request.target),
                });
            }
        }

        let target = request.target.clone();
        let attach_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (active.session_name == target && !active.suspended).then_some(*pid)
                })
                .collect::<Vec<_>>()
        };
        for attach_pid in attach_pids {
            if let Err(error) = self.lock_attached_client(attach_pid).await {
                return Response::Error(ErrorResponse { error });
            }
        }
        self.refresh_attached_session(&request.target).await;
        Response::LockSession(LockSessionResponse {
            target: request.target,
        })
    }

    pub(in crate::handler) async fn handle_lock_client(
        &self,
        requester_pid: u32,
        request: rmux_proto::LockClientRequest,
    ) -> Response {
        let attach_pid = match self
            .resolve_lock_client_pid(requester_pid, &request.target_client)
            .await
        {
            Ok(attach_pid) => attach_pid,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let session_name = match self.lock_attached_client(attach_pid).await {
            Ok(Some(session_name)) => Some(session_name),
            Ok(None) => None,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        if let Some(session_name) = session_name {
            self.refresh_attached_session(&session_name).await;
        }
        Response::LockClient(LockClientResponse {
            target_client: request.target_client,
        })
    }

    pub(in crate::handler) async fn resolve_lock_client_pid(
        &self,
        requester_pid: u32,
        target_client: &str,
    ) -> Result<u32, RmuxError> {
        let target_client = normalize_target_client(target_client);
        if target_client == "=" {
            return self
                .resolve_attached_client_pid(requester_pid, "lock-client")
                .await;
        }

        let active_attach = self.active_attach.lock().await;
        if let Ok(pid) = target_client.parse::<u32>() {
            if active_attach.by_pid.contains_key(&pid) {
                return Ok(pid);
            }
            return Err(RmuxError::Server(format!(
                "lock-client client {pid} is not attached"
            )));
        }

        let attach_pids = active_attach.by_pid.keys().copied().collect::<Vec<_>>();
        drop(active_attach);

        attach_pids
            .into_iter()
            .find(|attach_pid| attached_client_matches_target(*attach_pid, target_client))
            .ok_or_else(|| RmuxError::Server(format!("can't find client: {target_client}")))
    }

    pub(in crate::handler) async fn session_lock_command(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> String {
        let state = self.state.lock().await;
        state
            .options
            .resolve(Some(session_name), OptionName::LockCommand)
            .or_else(|| state.options.resolve(None, OptionName::LockCommand))
            .map(str::to_owned)
            .unwrap_or_default()
    }

    pub(in crate::handler) async fn lock_attached_client(
        &self,
        attach_pid: u32,
    ) -> Result<Option<rmux_proto::SessionName>, RmuxError> {
        let session_name = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid) else {
                return Ok(None);
            };
            if active.suspended {
                return Ok(None);
            }
            active.session_name.clone()
        };
        let command = self.session_lock_command(&session_name).await;
        if command.is_empty() {
            return Ok(None);
        }
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Ok(None);
        };
        if active.suspended {
            return Ok(None);
        }
        active.suspended = true;
        if active
            .control_tx
            .send(AttachControl::Lock(command))
            .is_err()
        {
            active_attach.by_pid.remove(&attach_pid);
            return Ok(None);
        }
        Ok(Some(active.session_name.clone()))
    }
}
