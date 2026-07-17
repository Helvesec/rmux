use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
};
use rmux_proto::{
    AttachSessionResponse, ErrorResponse, Response, RmuxError, CAPABILITY_ATTACH_RENDER,
};
use tokio::sync::mpsc;

use super::super::{
    attach_support::attach_target_for_session, client_environment_snapshot,
    effective_client_terminal_context, parse_client_flags, update_environment_from_client,
    validate_expected_attach_identity, RequestHandler,
};
use super::switching::SwitchManagedClientIdentity;
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::HandleOutcome;

impl RequestHandler {
    pub(in crate::handler) async fn handle_attach_session(
        &self,
        requester_pid: u32,
        request: rmux_proto::AttachSessionRequest,
    ) -> HandleOutcome {
        self.handle_attach_session_ext(
            requester_pid,
            AttachSessionExtRequest {
                target: Some(request.target),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_attach_session_ext(
        &self,
        requester_pid: u32,
        request: AttachSessionExtRequest,
    ) -> HandleOutcome {
        let target_spec = request.target.as_ref().map(ToString::to_string);
        self.handle_attach_session_ext2(
            requester_pid,
            AttachSessionExt2Request {
                target: request.target,
                target_spec,
                detach_other_clients: request.detach_other_clients,
                kill_other_clients: request.kill_other_clients,
                read_only: request.read_only,
                skip_environment_update: request.skip_environment_update,
                flags: request.flags,
                working_directory: None,
                client_terminal: rmux_proto::ClientTerminalContext::default(),
                client_size: None,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_attach_session_ext2(
        &self,
        requester_pid: u32,
        request: AttachSessionExt2Request,
    ) -> HandleOutcome {
        self.handle_attach_session_ext2_inner(requester_pid, request, false)
            .await
    }

    pub(in crate::handler) async fn handle_attach_session_ext3(
        &self,
        requester_pid: u32,
        request: AttachSessionExt3Request,
    ) -> HandleOutcome {
        let (request, attach_capabilities) = request.into_ext2_and_capabilities();
        let render_stream = attach_capabilities
            .iter()
            .any(|capability| capability == CAPABILITY_ATTACH_RENDER);
        self.handle_attach_session_ext2_inner(requester_pid, request, render_stream)
            .await
    }

    async fn handle_attach_session_ext2_inner(
        &self,
        requester_pid: u32,
        request: AttachSessionExt2Request,
        render_stream: bool,
    ) -> HandleOutcome {
        let expected_attach = match validate_expected_attach_identity(self, requester_pid).await {
            Ok(identity) => identity,
            Err(error) => {
                return HandleOutcome::response(Response::Error(ErrorResponse { error }));
            }
        };
        let mut session_name = match request.target {
            Some(session_name) => session_name,
            None => match self.preferred_session_name().await {
                Ok(session_name) => session_name,
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            },
        };
        if let Some(target_spec) = request.target_spec.as_deref() {
            let current_session = self.current_session_candidate(requester_pid).await;
            let current_session = match current_session {
                Some(session_name) => self.switch_session_identity(session_name).await.ok(),
                None => None,
            };
            match self
                .apply_switch_target(
                    target_spec,
                    current_session.as_ref(),
                    rmux_core::TargetFindFlags::PREFER_UNATTACHED,
                    false,
                )
                .await
            {
                Ok(next_session) => session_name = next_session.session_name,
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            }
        }
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let flags = match parse_client_flags(request.flags.as_ref(), request.read_only) {
            Ok(flags) => flags,
            Err(error) => return HandleOutcome::response(Response::Error(ErrorResponse { error })),
        };
        if let Some(template) = request.working_directory.as_deref() {
            if let Err(error) = self
                .update_session_cwd_from_template(&session_name, template)
                .await
            {
                return HandleOutcome::response(Response::Error(ErrorResponse { error }));
            }
        }
        let client_environment = client_environment_snapshot(requester_pid);
        let client_terminal = effective_client_terminal_context(
            client_environment.as_ref(),
            &request.client_terminal,
        );
        let terminal_context = OuterTerminalContext::from_environment(client_environment.as_ref())
            .with_client_terminal(&client_terminal);
        if let Some(client_environment) = client_environment.as_ref() {
            if !request.skip_environment_update {
                let mut state = self.state.lock().await;
                update_environment_from_client(&mut state, &session_name, client_environment);
            }
        }
        if request.detach_other_clients || request.kill_other_clients {
            self.detach_other_attach_clients_for_session(
                &session_name,
                requester_pid,
                request.kill_other_clients,
            )
            .await;
        }
        if let Err(error) = self
            .resize_session_for_attach_client(&session_name, request.client_size, flags)
            .await
        {
            return HandleOutcome::response(Response::Error(ErrorResponse { error }));
        }
        let managed_client = match expected_attach {
            Some(identity) => Some(SwitchManagedClientIdentity::Attach {
                pid: identity.attach_pid(),
                attach_id: identity.attach_id(),
            }),
            None => self.managed_client_for_pid(requester_pid).await,
        };
        if let Some(client) = managed_client {
            #[cfg(test)]
            super::switching::pause_after_switch_target_identity_capture(&session_name).await;
            if let SwitchManagedClientIdentity::Attach {
                pid: attach_pid,
                attach_id,
            } = client
            {
                if let Err(error) = self
                    .set_attached_client_flags(attach_pid, attach_id, flags)
                    .await
                {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            } else if request.read_only || request.flags.is_some() {
                return HandleOutcome::response(Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "attach-session client flags are not available for control clients"
                            .to_owned(),
                    ),
                }));
            }

            return HandleOutcome::response(
                self.switch_managed_client_to_session(
                    requester_pid,
                    client,
                    session_name,
                    request.skip_environment_update,
                )
                .await,
            );
        }
        let attached_count = self.attached_count(&session_name).await.saturating_add(1);
        let (session_id, target) = {
            let state = self.state.lock().await;
            let Some(session_id) = state
                .sessions
                .session(&session_name)
                .map(rmux_core::Session::id)
            else {
                return HandleOutcome::response(Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(session_name.to_string()),
                }));
            };
            match attach_target_for_session(
                &state,
                &session_name,
                attached_count,
                &terminal_context,
                &self.socket_path(),
            ) {
                Ok(target) => (session_id, target),
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            }
        };

        let (control_tx, control_rx) = mpsc::unbounded_channel();

        let upgrade = crate::pane_io::AttachSessionUpgrade::new(
            session_id,
            target,
            control_tx,
            control_rx,
            flags,
            request.client_size,
            render_stream,
        );
        HandleOutcome::attach(
            Response::AttachSession(AttachSessionResponse { session_name }),
            upgrade,
        )
    }
}
