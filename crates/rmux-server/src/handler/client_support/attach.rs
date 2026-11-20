use rmux_proto::request::{
    AttachSessionExt2Request, AttachSessionExt3Request, AttachSessionExtRequest,
};
use rmux_proto::{
    AttachSessionResponse, ErrorResponse, Response, RmuxError, CAPABILITY_ATTACH_RENDER,
    CAPABILITY_ATTACH_SERVER_EXITED,
};
use tokio::sync::mpsc;

use super::super::client_runtime_support::{AttachingClient, ListClientSnapshot};
use super::super::{
    attach_support::{attach_target_for_session, AttachSessionRenderOptions},
    client_environment_snapshot, effective_client_terminal_context, parse_client_flags,
    update_environment_from_client, validate_expected_attach_identity, RequestHandler,
};
use super::switching::{ResolvedSwitchTarget, SwitchManagedClientIdentity};
use crate::client_format::sort_client_loop_snapshots;
use crate::control::ControlFlagChange;
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
        self.handle_attach_session_ext2_inner(requester_pid, request, false, false)
            .await
    }

    pub(in crate::handler) async fn handle_attach_session_ext3(
        &self,
        requester_pid: u32,
        request: AttachSessionExt3Request,
    ) -> HandleOutcome {
        let (request, attach_capabilities) = request.into_ext2_and_capabilities();
        let opted_in = |id: &str| {
            attach_capabilities
                .iter()
                .any(|capability| capability == id)
        };
        let render_stream = opted_in(CAPABILITY_ATTACH_RENDER);
        let server_exit_notice = opted_in(CAPABILITY_ATTACH_SERVER_EXITED);
        self.handle_attach_session_ext2_inner(
            requester_pid,
            request,
            render_stream,
            server_exit_notice,
        )
        .await
    }

    async fn handle_attach_session_ext2_inner(
        &self,
        requester_pid: u32,
        request: AttachSessionExt2Request,
        render_stream: bool,
        server_exit_notice: bool,
    ) -> HandleOutcome {
        let expected_attach = match validate_expected_attach_identity(self, requester_pid).await {
            Ok(identity) => identity,
            Err(error) => {
                return HandleOutcome::response(Response::Error(ErrorResponse { error }));
            }
        };
        let managed_client = match expected_attach {
            Some(identity) => Some(SwitchManagedClientIdentity::Attach {
                pid: identity.attach_pid(),
                attach_id: identity.attach_id(),
            }),
            None => self.managed_client_for_pid(requester_pid).await,
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
        let mut resolved_target = None;
        if let Some(target_spec) = request.target_spec.as_deref() {
            let current_session = self.current_session_candidate(requester_pid).await;
            let current_session = match current_session {
                Some(session_name) => self.switch_session_identity(session_name).await.ok(),
                None => None,
            };
            let resolved = match managed_client {
                Some(client) => {
                    // A client move carries selection and identity into the
                    // same locked commit as its frame, not an earlier mutation
                    // followed by a fresh lookup of only the session name.
                    self.apply_switch_target_for_client_identity(
                        target_spec,
                        current_session.as_ref(),
                        rmux_core::TargetFindFlags::PREFER_UNATTACHED,
                        false,
                        client,
                    )
                    .await
                }
                None => self
                    .apply_switch_target(
                        target_spec,
                        current_session.as_ref(),
                        rmux_core::TargetFindFlags::PREFER_UNATTACHED,
                        false,
                    )
                    .await
                    .map(|session| ResolvedSwitchTarget {
                        session,
                        selection: None,
                    }),
            };
            match resolved {
                Ok(target) => {
                    session_name = target.session.session_name.clone();
                    resolved_target = Some(target);
                }
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            }
        }
        if managed_client.is_some() && resolved_target.is_none() {
            match self.switch_session_identity(session_name.clone()).await {
                Ok(session) => {
                    resolved_target = Some(ResolvedSwitchTarget {
                        session,
                        selection: None,
                    })
                }
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }))
                }
            }
        }
        #[cfg(windows)]
        self.wait_for_windows_deferred_all_pane_pids().await;
        let control_client = matches!(
            managed_client,
            Some(SwitchManagedClientIdentity::Control { .. })
        );
        let flags = match parse_attach_client_flags(
            request.flags.as_ref(),
            request.read_only,
            control_client,
        ) {
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
        let mut resize_publication = None;
        if !matches!(
            managed_client,
            Some(SwitchManagedClientIdentity::Attach { .. })
        ) {
            let resize_client = super::AttachResizeClient::new(request.client_size, flags);
            let resize = if let Some(target) = managed_client.and(resolved_target.as_ref()) {
                self.resize_session_geometry_for_attach_client(
                    &session_name,
                    resize_client,
                    Some(target.session.session_id),
                    target.selection.as_ref(),
                    false,
                )
                .await
            } else {
                self.resize_session_for_attach_client(&session_name, resize_client)
                    .await
            };
            match resize {
                Ok(publication) => resize_publication = publication,
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }))
                }
            }
        }
        if let Some(client) = managed_client {
            let target = resolved_target.expect("managed attach captures a stable target");
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
            }

            return HandleOutcome::response(
                self.switch_managed_client_to_session_with_control_flags(
                    requester_pid,
                    client,
                    target.session,
                    target.selection,
                    request.skip_environment_update,
                    ControlFlagChange::Attach {
                        flags: request.flags.unwrap_or_default(),
                        read_only: request.read_only,
                    },
                )
                .await,
            );
        }
        let attached_count = self.attached_count(&session_name).await.saturating_add(1);
        let attach_order = self.reserve_attach_id().await;
        let mut client_loop_snapshots = self.client_loop_snapshots().await;
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
                order: attach_order,
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
            client_loop_snapshots.push(attaching_client.client_loop_snapshot());
            sort_client_loop_snapshots(&mut client_loop_snapshots);
            match attach_target_for_session(
                &state,
                &session_name,
                AttachSessionRenderOptions {
                    attached_count,
                    terminal_context: &terminal_context,
                    // A fresh client's outer terminal shows nothing of ours yet, so
                    // this frame carries the first title; registration remembers it.
                    previous_title: None,
                    client: Some(&attaching_client),
                    socket_path: &self.socket_path(),
                    client_loop_snapshots: Some(&client_loop_snapshots),
                },
            ) {
                Ok(target) => (session_id, target),
                Err(error) => {
                    return HandleOutcome::response(Response::Error(ErrorResponse { error }));
                }
            }
        };

        let (control_tx, control_rx) = mpsc::unbounded_channel();

        let mut upgrade = crate::pane_io::AttachSessionUpgrade::new(
            attach_order,
            session_id,
            target,
            (control_tx, control_rx),
            flags,
            request.client_size,
            render_stream,
        );
        upgrade.resize_publication = resize_publication;
        upgrade.server_exit_notice = server_exit_notice;
        // The listener publishes the client/session transition after it has
        // installed this reserved native attach. That registration is the
        // commit point for its initial pane focus, which must drain before the
        // client/session and client-attached lifecycle events. Queue-based
        // control consumers publish the same transition after installing
        // their own attachment.
        HandleOutcome::attach(
            Response::AttachSession(AttachSessionResponse { session_name }),
            upgrade,
        )
    }
}

/// tmux 3.7b `server_client_control_flags()`: a control client also takes
/// `no-output`, `wait-exit`, `pause-after` and `pause-after=N` (each also as
/// `!flag`). Its session commit applies them to [`crate::control::ControlClientFlags`].
fn is_control_only_attach_flag(token: &str) -> bool {
    let token = token.strip_prefix('!').unwrap_or(token);
    matches!(token, "no-output" | "wait-exit" | "pause-after")
        || token
            .strip_prefix("pause-after=")
            .is_some_and(|seconds| seconds.parse::<u32>().is_ok())
}

/// The shared client flags of an `attach-session -f` list; a control client's
/// own flags are left to its session commit, any other client rejects them.
///
/// A control client's refusal belongs here, before the caller applies `-c`,
/// the client environment, `-d`/`-x` or a resize: a refused attach changes
/// nothing, like an unknown flag.
fn parse_attach_client_flags(
    flags: Option<&Vec<String>>,
    read_only: bool,
    control_client: bool,
) -> Result<super::ClientFlags, RmuxError> {
    if !control_client {
        return parse_client_flags(flags, read_only);
    }
    let shared = flags.map(|values| {
        values
            .iter()
            .flat_map(|value| value.split(','))
            .filter(|token| !token.is_empty() && !is_control_only_attach_flag(token))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    let flags = parse_client_flags(shared.as_ref(), read_only)?;
    if !control_client_supports_attach_flags(flags) {
        return Err(RmuxError::Server(
            "attach-session client flags are not available for control clients".to_owned(),
        ));
    }
    Ok(flags)
}

/// A control client carries tmux's `CLIENT_IGNORESIZE` and `CLIENT_READONLY`
/// (`-r` sets both, as in tmux 3.7b `cmd_attach_session()`); it has no
/// `active-pane` or `no-detach-on-destroy` state to update yet.
fn control_client_supports_attach_flags(flags: super::ClientFlags) -> bool {
    let mut unsupported = flags;
    unsupported.remove(super::ClientFlags::IGNORESIZE);
    unsupported.remove(super::ClientFlags::READONLY);
    unsupported == super::ClientFlags::default()
}
