use rmux_core::LifecycleEvent;
use rmux_proto::{CommandOutput, ErrorResponse, Response, RmuxError, ServerAccessResponse};

use super::{ClientFlags, RequestHandler};
use crate::pane_io::AttachControl;
use crate::server_access::{resolve_user, validate_server_access_request, AccessMode};

impl RequestHandler {
    pub(in crate::handler) async fn handle_server_access(
        &self,
        request: rmux_proto::ServerAccessRequest,
    ) -> Response {
        if let Err(error) = validate_server_access_request(&request) {
            return Response::Error(ErrorResponse { error });
        }

        if request.list {
            let output = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .render_list();
            return Response::ServerAccess(ServerAccessResponse { output });
        }

        let Some(user) = request.user.as_deref() else {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("missing user argument".to_owned()),
            });
        };
        let resolved = match resolve_user(user) {
            Ok(resolved) => resolved,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let owner_uid = self
            .server_access
            .lock()
            .expect("server access mutex must not be poisoned")
            .owner_uid();
        if resolved.uid == 0 || resolved.uid == owner_uid {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server(format!(
                    "{} owns the server, can't change access",
                    resolved.name
                )),
            });
        }

        if request.deny {
            let exists = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .contains_uid(resolved.uid);
            if !exists {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("user {} not found", resolved.name)),
                });
            }
            self.disconnect_clients_by_uid(resolved.uid).await;
            if let Err(error) = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned")
                .remove_uid(resolved.uid)
            {
                return Response::Error(ErrorResponse { error });
            }
            return Response::ServerAccess(ServerAccessResponse {
                output: CommandOutput::from_stdout(Vec::new()),
            });
        }

        {
            let mut server_access = self
                .server_access
                .lock()
                .expect("server access mutex must not be poisoned");
            if request.add && server_access.contains_uid(resolved.uid) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(format!("user {} is already added", resolved.name)),
                });
            }
            let should_add = request.add
                || ((request.read_only || request.write)
                    && !server_access.contains_uid(resolved.uid));
            if should_add
                && let Err(error) = server_access.set_mode(resolved.uid, AccessMode::ReadWrite) {
                    return Response::Error(ErrorResponse { error });
                }
            if request.write
                && let Err(error) = server_access.set_mode(resolved.uid, AccessMode::ReadWrite) {
                    return Response::Error(ErrorResponse { error });
                }
            if request.read_only
                && let Err(error) = server_access.set_mode(resolved.uid, AccessMode::ReadOnly) {
                    return Response::Error(ErrorResponse { error });
                }
        }

        if request.write {
            self.update_live_access_mode(resolved.uid, true).await;
        } else if request.read_only {
            self.update_live_access_mode(resolved.uid, false).await;
        }

        Response::ServerAccess(ServerAccessResponse {
            output: CommandOutput::from_stdout(Vec::new()),
        })
    }

    pub(in crate::handler) async fn update_live_access_mode(&self, uid: u32, can_write: bool) {
        let mut sessions = Vec::new();
        {
            let mut active_attach = self.active_attach.lock().await;
            for active in active_attach.by_pid.values_mut() {
                if active.uid != uid {
                    continue;
                }
                active.can_write = can_write;
                if can_write {
                    active.flags.remove(ClientFlags::READONLY);
                    active.flags.remove(ClientFlags::IGNORESIZE);
                } else {
                    active.flags.insert(ClientFlags::READONLY);
                    active.flags.insert(ClientFlags::IGNORESIZE);
                }
                sessions.push(active.session_name.clone());
            }
        }
        {
            let mut active_control = self.active_control.lock().await;
            for active in active_control.by_pid.values_mut() {
                if active.uid == uid {
                    active.can_write = can_write;
                }
            }
        }

        sessions.sort_by_key(|session_name| session_name.to_string());
        sessions.dedup();
        for session_name in sessions {
            self.refresh_attached_session(&session_name).await;
        }
    }

    pub(in crate::handler) async fn disconnect_clients_by_uid(&self, uid: u32) {
        let attached = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter_map(|(pid, active)| {
                    (active.uid == uid).then_some((*pid, active.session_name.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (attach_pid, session_name) in attached {
            if self
                .send_attach_control(attach_pid, AttachControl::Detach, "server-access", None)
                .await
                .is_ok()
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(attach_pid.to_string()),
                })
                .await;
            }
        }

        let control_pids = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .filter_map(|(pid, active)| (active.uid == uid).then_some(*pid))
                .collect::<Vec<_>>()
        };
        for control_pid in control_pids {
            if let Ok(Some(session_name)) = self
                .exit_control_client(control_pid, Some("access not allowed".to_owned()))
                .await
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name,
                    client_name: Some(control_pid.to_string()),
                })
                .await;
            }
        }
    }
}
