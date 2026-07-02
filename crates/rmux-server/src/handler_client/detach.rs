use rmux_core::LifecycleEvent;
use rmux_proto::request::DetachClientExtRequest;
use rmux_proto::{DetachClientResponse, ErrorResponse, Response, RmuxError};

use crate::pane_io::AttachControl;

use super::super::{control_support::ManagedClient, RequestHandler};

impl RequestHandler {
    async fn detach_attach_client_with_mode(
        &self,
        attach_pid: u32,
        kill_on_detach: bool,
        exec_command: Option<String>,
        command_name: &str,
    ) -> Result<rmux_proto::SessionName, RmuxError> {
        let control = if let Some(command) = exec_command {
            let session_name = self
                .attached_session_name_for_command(attach_pid, command_name)
                .await?;
            let command = self
                .attach_shell_command_for_session(&session_name, command)
                .await?;
            AttachControl::DetachExecShellCommand(command)
        } else if kill_on_detach {
            AttachControl::DetachKill
        } else {
            AttachControl::Detach
        };
        let session_name = self
            .send_attach_control(attach_pid, control, command_name, None)
            .await?;
        self.reconcile_attached_session_size_and_emit(&session_name)
            .await?;
        Ok(session_name)
    }

    pub(in crate::handler) async fn detach_other_attach_clients_for_session(
        &self,
        session_name: &rmux_proto::SessionName,
        requester_pid: u32,
        kill_clients: bool,
    ) {
        let attach_pids = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_client_pids_for_session(session_name, Some(requester_pid))
        };

        for attach_pid in attach_pids {
            if let Ok(detached_session) = self
                .detach_attach_client_with_mode(attach_pid, kill_clients, None, "attach-session")
                .await
            {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name: detached_session,
                    client_name: Some(attach_pid.to_string()),
                })
                .await;
            }
        }
    }

    pub(in crate::handler) async fn handle_detach_client(&self, requester_pid: u32) -> Response {
        self.handle_detach_client_ext(
            requester_pid,
            DetachClientExtRequest {
                target_client: None,
                all_other_clients: false,
                target_session: None,
                kill_on_detach: false,
                exec_command: None,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_detach_client_ext(
        &self,
        requester_pid: u32,
        request: DetachClientExtRequest,
    ) -> Response {
        if request.target_session.is_some() && request.target_client.is_some() {
            return Response::Error(ErrorResponse {
                error: RmuxError::Server("detach-client accepts -t or -s, not both".to_owned()),
            });
        }

        if let Some(session_name) = request.target_session.as_ref() {
            let attach_pids = {
                let active_attach = self.active_attach.lock().await;
                active_attach.attached_client_pids_for_session(session_name, None)
            };
            for attach_pid in attach_pids {
                if self
                    .detach_attach_client_with_mode(
                        attach_pid,
                        request.kill_on_detach,
                        request.exec_command.clone(),
                        "detach-client",
                    )
                    .await
                    .is_ok()
                {
                    self.emit(LifecycleEvent::ClientDetached {
                        session_name: session_name.clone(),
                        client_name: Some(attach_pid.to_string()),
                    })
                    .await;
                }
            }
            let control_pids = self
                .detach_control_clients_for_session(session_name, None)
                .await;
            for control_pid in control_pids {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name: session_name.clone(),
                    client_name: Some(control_pid.to_string()),
                })
                .await;
            }
            return Response::DetachClient(DetachClientResponse);
        }

        if request.all_other_clients {
            let keep_pid = match self
                .resolve_target_attach_client_pid(
                    requester_pid,
                    request.target_client.as_deref(),
                    "detach-client",
                )
                .await
            {
                Ok(pid) => pid,
                Err(error) => return Response::Error(ErrorResponse { error }),
            };
            let attach_pids = {
                let active_attach = self.active_attach.lock().await;
                active_attach.attached_client_pids_except(keep_pid)
            };
            for attach_pid in attach_pids {
                if let Ok(session_name) = self
                    .detach_attach_client_with_mode(
                        attach_pid,
                        request.kill_on_detach,
                        request.exec_command.clone(),
                        "detach-client",
                    )
                    .await
                {
                    self.emit(LifecycleEvent::ClientDetached {
                        session_name,
                        client_name: Some(attach_pid.to_string()),
                    })
                    .await;
                }
            }
            return Response::DetachClient(DetachClientResponse);
        }

        let client = match self
            .resolve_target_managed_client(
                requester_pid,
                request.target_client.as_deref(),
                "detach-client",
            )
            .await
        {
            Ok(client) => client,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        match client {
            ManagedClient::Attach(attach_pid) => match self
                .detach_attach_client_with_mode(
                    attach_pid,
                    request.kill_on_detach,
                    request.exec_command,
                    "detach-client",
                )
                .await
            {
                Ok(session_name) => {
                    self.emit(LifecycleEvent::ClientDetached {
                        session_name,
                        client_name: Some(attach_pid.to_string()),
                    })
                    .await;
                    Response::DetachClient(DetachClientResponse)
                }
                Err(error) => Response::Error(ErrorResponse { error }),
            },
            ManagedClient::Control(control_pid) => {
                match self.exit_control_client(control_pid, None).await {
                    Ok(Some(session_name)) => {
                        self.emit(LifecycleEvent::ClientDetached {
                            session_name,
                            client_name: Some(control_pid.to_string()),
                        })
                        .await;
                        Response::DetachClient(DetachClientResponse)
                    }
                    Ok(None) => Response::DetachClient(DetachClientResponse),
                    Err(error) => Response::Error(ErrorResponse { error }),
                }
            }
        }
    }
}
