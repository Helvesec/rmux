use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
};
use rmux_proto::{
    AttachSessionResponse, ErrorResponse, Response, RmuxError, CAPABILITY_ATTACH_RENDER,
};
use tokio::sync::mpsc;

use super::super::client_runtime_support::{AttachingClient, ListClientSnapshot};
use super::super::{
    attach_support::attach_target_for_session, client_environment_snapshot,
    effective_client_terminal_context, parse_client_flags, update_environment_from_client,
    validate_expected_attach_identity, RequestHandler,
};
use super::switching::SwitchManagedClientIdentity;
use crate::client_names::attached_client_name;
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
        let managed_client = match expected_attach {
            Some(identity) => Some(SwitchManagedClientIdentity::Attach {
                pid: identity.attach_pid(),
                attach_id: identity.attach_id(),
            }),
            None => self.managed_client_for_pid(requester_pid).await,
        };
        // An already-attached client is *moving*, and its switch commit performs
        // that whole move — size selection, window and PTY mutation, and the
        // delivery of the frame — inside one `state` -> `active_attach` ->
        // `active_control` region. Sizing the destination here as well would be a
        // second write to the same shared window from outside that region: it
        // cannot see whether the captured generation can still receive the
        // switch, and the commit that decides so runs afterwards. The commit
        // re-selects from that same registration, so on the path where the
        // command succeeds this write is also redundant.
        //
        // A first attach and a control client own no attach registration and
        // deliver no switch frame, so their resize is the only one there is.
        if !matches!(
            managed_client,
            Some(SwitchManagedClientIdentity::Attach { .. })
        ) {
            if let Err(error) = self
                .resize_session_for_attach_client(
                    &session_name,
                    super::AttachResizeClient::new(request.client_size, flags),
                )
                .await
            {
                return HandleOutcome::response(Response::Error(ErrorResponse { error }));
            }
        }
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
            } else if flags == super::ClientFlags::IGNORESIZE {
                self.set_control_client_ignore_size(requester_pid).await;
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
        // The identity the listener authenticated for this connection and is
        // about to register this client under, so the frame rendered below and
        // that registration describe one client (issue #182). An ambiguous
        // requester is rejected here, before any frame or registration exists.
        let (requester_uid, requester_user) = match self.attaching_client_identity(requester_pid) {
            Ok(identity) => identity,
            Err(error) => return HandleOutcome::response(Response::Error(ErrorResponse { error })),
        };
        let (session_id, target) = {
            let state = self.state.lock().await;
            let Some(session) = state.sessions.session(&session_name) else {
                return HandleOutcome::response(Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(session_name.to_string()),
                }));
            };
            let session_id = session.id();
            // Registration has not published this client yet, so its record is
            // built from the request: the first frame's title must already
            // resolve its own `#{client_*}` values (issue #182).
            let attaching_client = ListClientSnapshot::for_attaching_client(AttachingClient {
                pid: requester_pid,
                session_name: &session_name,
                session_id,
                // The same anchor `register_attach_identity` is about to store,
                // so this frame and every later one expand `#{client_height}`
                // to one value. The session's window size is content geometry
                // with the status rows already off it; handing it to a sizeless
                // client here would make its first title alone report the
                // status-subtracted height.
                size: request
                    .client_size
                    .unwrap_or_else(|| session.terminal_size()),
                terminal_context: &terminal_context,
                flags,
                uid: requester_uid,
                user: requester_user,
                activity_at: crate::handler::current_client_activity_timestamp(),
            });
            match attach_target_for_session(
                &state,
                &session_name,
                attached_count,
                &terminal_context,
                // A fresh client's outer terminal shows nothing of ours yet, so
                // this frame carries the first title; registration remembers it.
                None,
                Some(&attaching_client),
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
        // Frozen tmux 3.7b reports a newly attached PTY by its tty path before
        // the layout change caused by its geometry. Publish at the attach
        // commit, while that resize is still queued, rather than at the later
        // listener registration. Existing PTY and control clients took the
        // managed-client arm above and publish from their switch commit.
        self.emit_client_session_changed(
            attached_client_name(requester_pid),
            session_name.clone(),
            session_id,
        )
        .await;
        HandleOutcome::attach(
            Response::AttachSession(AttachSessionResponse { session_name }),
            upgrade,
        )
    }
}
