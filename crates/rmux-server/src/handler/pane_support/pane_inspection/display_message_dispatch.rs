use rmux_core::formats::{FormatContext, DEFAULT_DISPLAY_MESSAGE_FORMAT};
use rmux_proto::{
    CommandOutput, DisplayMessageResponse, ErrorResponse, Response, RmuxError, Target,
};

use super::super::super::control_support::current_control_queue_identity;
use super::super::super::target_support::{inherited_pane_target, requester_environment_context};
use super::super::super::{current_expected_attach_identity, ListClientSnapshot, RequestHandler};
#[cfg(windows)]
use super::super::pane_deferred_wait::format_references_pane_pid;
use super::{
    display_message_context, display_message_context_target, display_time, lone_session_name,
    target_resolves_to_pane_id, with_runtime_client_values, DisplayMessageClient,
    DisplayMessageInvocation,
};
use crate::control_notifications::format_control_message_line;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::handler::scripting_support::{queued_display_target_client, QueuedDisplayTargetClient};

impl RequestHandler {
    pub(super) async fn handle_display_message_inner(
        &self,
        requester_pid: u32,
        invocation: DisplayMessageInvocation,
    ) -> Response {
        let DisplayMessageInvocation {
            target,
            print,
            message,
            target_client,
            expected_pane_id,
            empty_target_context,
            route_control_to_target_session,
            duration_ms,
            ignore_input,
        } = invocation;
        let explicit_display_client = match queued_display_target_client() {
            Some(QueuedDisplayTargetClient::Attached(identity)) => {
                Some(DisplayMessageClient::Attached(identity))
            }
            Some(QueuedDisplayTargetClient::Control(identity)) => {
                Some(DisplayMessageClient::Control(identity))
            }
            Some(QueuedDisplayTargetClient::Missing) if print => None,
            Some(QueuedDisplayTargetClient::Missing) => {
                return Response::DisplayMessage(DisplayMessageResponse::no_output());
            }
            Some(QueuedDisplayTargetClient::ResolutionError(error)) => {
                return Response::Error(ErrorResponse { error });
            }
            None => match target_client.as_deref() {
                Some(target_client) => match self
                    .find_named_client(requester_pid, target_client, "display-message")
                    .await
                {
                    Ok(Some(client)) => {
                        match self.display_message_client_from_managed(client).await {
                            Some(client) => Some(client),
                            None if print => None,
                            None => {
                                return Response::DisplayMessage(
                                    DisplayMessageResponse::no_output(),
                                );
                            }
                        }
                    }
                    Ok(None) if print => None,
                    Ok(None) => {
                        return Response::DisplayMessage(DisplayMessageResponse::no_output());
                    }
                    Err(error) => return Response::Error(ErrorResponse { error }),
                },
                None => None,
            },
        };
        let requester_is_control = self.is_control_client(requester_pid).await;
        let job_owner = if let Some(identity) = current_control_queue_identity(requester_pid) {
            crate::status_jobs::StatusJobOwner::Command {
                control: true,
                pid: requester_pid,
                generation: identity.control_id(),
            }
        } else if let Some(identity) = current_expected_attach_identity() {
            crate::status_jobs::StatusJobOwner::Command {
                control: false,
                pid: requester_pid,
                generation: identity.attach_id(),
            }
        } else {
            crate::status_jobs::StatusJobOwner::Ephemeral(crate::status_jobs::new_job_owner_id())
        };
        let captured_target_client_session = match explicit_display_client {
            Some(identity) => match self.display_message_client_session(identity).await {
                Ok(session) => session,
                Err(error) => return Response::Error(ErrorResponse { error }),
            },
            None => None,
        };
        let exact_target_client_session = target
            .is_none()
            .then(|| captured_target_client_session.clone())
            .flatten();
        let resolved_requester_client = if let Some(identity) = current_expected_attach_identity() {
            Some(DisplayMessageClient::Attached(identity))
        } else if let Some(identity) = current_control_queue_identity(requester_pid) {
            Some(DisplayMessageClient::Control(identity))
        } else {
            match self
                .resolve_target_managed_client(requester_pid, None, "display-message")
                .await
            {
                Ok(client) => self.display_message_client_from_managed(client).await,
                Err(_) => None,
            }
        };
        let requester_environment_target = if target.is_some() || empty_target_context {
            None
        } else {
            let socket_path = self.socket_path();
            let requester_environment = requester_environment_context(requester_pid, &socket_path);
            let state = self.state.lock().await;
            match inherited_pane_target(&state.sessions, requester_environment, None) {
                Ok(target) => target,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        };
        let format_fallback_client = match resolved_requester_client {
            Some(identity) => Some(identity),
            None if requester_is_control => None,
            None => {
                let preferred_session = requester_environment_target
                    .as_ref()
                    .map(Target::session_name);
                match self.recent_display_message_client(preferred_session).await {
                    Some(identity) => Some(identity),
                    None if preferred_session.is_some() => {
                        self.recent_display_message_client(None).await
                    }
                    None => None,
                }
            }
        };
        let display_client = if route_control_to_target_session {
            None
        } else {
            explicit_display_client.or(format_fallback_client)
        };
        let display_client_session = match display_client {
            Some(identity) => match self.display_message_client_session(identity).await {
                Ok(session) => session,
                Err(error) if explicit_display_client.is_some() => {
                    return Response::Error(ErrorResponse { error });
                }
                Err(_) => None,
            },
            None => None,
        };
        let format_client = resolved_requester_client
            .filter(|_| print && expected_pane_id.is_some())
            .or(match (target.as_ref(), display_client_session.as_ref()) {
                (Some(target), Some((display_session, _)))
                    if target.session_name() == display_session =>
                {
                    display_client
                }
                (Some(target), _) => self
                    .recent_display_message_client(Some(target.session_name()))
                    .await
                    .or(format_fallback_client)
                    .or(display_client),
                (None, _) => format_fallback_client.or(display_client),
            });
        let client_snapshots = self.list_clients_snapshot().await;
        let client_loop_snapshots = client_snapshots
            .iter()
            .map(ListClientSnapshot::client_loop_snapshot)
            .collect::<Vec<_>>();
        let format_client_snapshot = match format_client {
            Some(DisplayMessageClient::Attached(identity)) => client_snapshots
                .iter()
                .find(|client| {
                    !client.control
                        && client.pid == identity.attach_pid()
                        && client.order == identity.attach_id()
                })
                .cloned(),
            Some(DisplayMessageClient::Control(identity)) => client_snapshots
                .iter()
                .find(|client| {
                    client.control
                        && client.pid == identity.requester_pid()
                        && client.order == identity.control_id()
                })
                .cloned(),
            None => None,
        };
        let attached_session_name = if target.is_none() {
            format_client_snapshot
                .as_ref()
                .and_then(|client| client.session_name.clone())
        } else {
            None
        };
        let fallback_session_name = if attached_session_name.is_some() {
            attached_session_name
        } else if let Some(target) = requester_environment_target.as_ref() {
            Some(target.session_name().clone())
        } else if requester_is_control {
            self.control_session_name(requester_pid).await
        } else {
            None
        };

        if target.is_none() && fallback_session_name.is_none() && !print && !requester_is_control {
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        }

        let mut session_name = target
            .as_ref()
            .map(|target| target.session_name().clone())
            .or_else(|| {
                requester_environment_target
                    .as_ref()
                    .map(|target| target.session_name().clone())
            })
            .or(fallback_session_name);
        let template = message.as_deref().unwrap_or(DEFAULT_DISPLAY_MESSAGE_FORMAT);
        let mut uses_lone_session_print_context = false;

        if empty_target_context {
            if !print {
                return Response::DisplayMessage(DisplayMessageResponse::no_output());
            }
            let expanded = {
                let state = self.state.lock().await;
                let mut runtime =
                    RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
                runtime = runtime.with_client_loop_snapshots(&client_loop_snapshots);
                if let Some(client) = format_client_snapshot.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
                let runtime = runtime
                    .with_socket_path(&self.socket_path())
                    .with_format_jobs(job_owner.clone(), std::time::Duration::from_secs(1));
                render_runtime_template(template, &runtime, true)
            };
            return Response::DisplayMessage(DisplayMessageResponse::from_output(
                CommandOutput::from_stdout(format!("{expanded}\n").into_bytes()),
            ));
        }

        if print && session_name.is_none() {
            session_name = {
                let state = self.state.lock().await;
                lone_session_name(&state.sessions)
            };
            uses_lone_session_print_context = session_name.is_some();
        }
        if print && session_name.is_none() {
            session_name = self.preferred_session_name().await.ok();
        }

        if print && session_name.is_none() {
            let expanded = {
                let state = self.state.lock().await;
                let mut runtime =
                    RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
                runtime = runtime.with_client_loop_snapshots(&client_loop_snapshots);
                if let Some(client) = format_client_snapshot.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
                let runtime = runtime
                    .with_socket_path(&self.socket_path())
                    .with_format_jobs(job_owner.clone(), std::time::Duration::from_secs(1));
                render_runtime_template(template, &runtime, true)
            };
            return Response::DisplayMessage(DisplayMessageResponse::from_output(
                CommandOutput::from_stdout(format!("{expanded}\n").into_bytes()),
            ));
        }

        let Some(session_name) = session_name else {
            let expanded = {
                let state = self.state.lock().await;
                let mut runtime =
                    RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
                runtime = runtime.with_client_loop_snapshots(&client_loop_snapshots);
                if let Some(client) = format_client_snapshot.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
                let runtime = runtime
                    .with_socket_path(&self.socket_path())
                    .with_format_jobs(job_owner.clone(), std::time::Duration::from_secs(1));
                render_runtime_template(template, &runtime, true)
            };
            self.send_control_response_notification_to(
                requester_pid,
                format_control_message_line(&expanded),
            )
            .await;
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        };
        let context_target =
            display_message_context_target(target, requester_environment_target, &session_name);
        #[cfg(windows)]
        if format_references_pane_pid(Some(template)) {
            self.wait_for_windows_deferred_target_pane_pids(&context_target)
                .await;
        }
        let attached_count = self.attached_count(&session_name).await;

        let (expanded, duration, display_session_id) = {
            let mut state = self.state.lock().await;
            if exact_target_client_session
                .as_ref()
                .is_some_and(|(expected_name, expected_id)| {
                    state
                        .sessions
                        .session(expected_name)
                        .map(|session| session.id())
                        != Some(*expected_id)
                })
            {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "target client session changed before message delivery".to_owned(),
                    ),
                });
            }
            let target_identity = match &context_target {
                Target::Session(target) => {
                    crate::handler::require_expected_session_identity(&state, target)
                }
                Target::Window(target) => {
                    crate::handler::require_expected_window_identity(&state, target)
                }
                Target::Pane(target) => {
                    crate::handler::require_expected_pane_identity(&state, target)
                }
            };
            if let Err(error) = target_identity {
                return Response::Error(ErrorResponse { error });
            }
            if expected_pane_id.is_some_and(|pane_id| {
                !target_resolves_to_pane_id(&state, &context_target, pane_id)
            }) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server(
                        "target pane changed before message delivery".to_owned(),
                    ),
                });
            }
            if let Err(error) = state.refresh_format_target_exit_status(&context_target) {
                return Response::Error(ErrorResponse { error });
            }
            let (format_session, mut context) =
                match display_message_context(&state, &context_target, attached_count) {
                    Ok(context) => context,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
            if let Some(client) = format_client_snapshot.as_ref() {
                context = with_runtime_client_values(context, client);
            }
            context = context.with_client_loop_snapshots(&client_loop_snapshots);
            if uses_lone_session_print_context {
                context = context.without_session_size();
            }
            context = context.with_named_value(
                "socket_path",
                self.socket_path().to_string_lossy().into_owned(),
            );
            context =
                context.with_format_jobs(job_owner.clone(), std::time::Duration::from_secs(1));
            let expanded = render_runtime_template(template, &context, true);

            if print {
                return Response::DisplayMessage(DisplayMessageResponse::from_output(
                    CommandOutput::from_stdout(format!("{expanded}\n").into_bytes()),
                ));
            }

            let display_session_name = display_client_session
                .as_ref()
                .map(|(name, _)| name)
                .unwrap_or(&session_name);
            let display_session = match (&display_client_session, display_client) {
                (Some((display_session_name, display_session_id)), Some(_)) => {
                    let Some(display_session) = state
                        .sessions
                        .session(display_session_name)
                        .filter(|session| session.id() == *display_session_id)
                    else {
                        return Response::DisplayMessage(DisplayMessageResponse::no_output());
                    };
                    display_session
                }
                _ => format_session,
            };
            (
                expanded,
                duration_ms.map_or_else(
                    || {
                        let duration = display_time(&state.options, display_session_name);
                        (!duration.is_zero()).then_some(duration)
                    },
                    |duration| {
                        (duration.get() != 0)
                            .then(|| std::time::Duration::from_millis(u64::from(duration.get())))
                    },
                ),
                display_session.id(),
            )
        };

        if requester_is_control && display_client.is_none() && !route_control_to_target_session {
            self.send_control_response_notification_to(
                requester_pid,
                format_control_message_line(&expanded),
            )
            .await;
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        }

        let input_policy = if ignore_input && duration.is_some() {
            crate::handler::attach_support::TransientMessageInputPolicy::IgnoreUntilExpiry
        } else {
            crate::handler::attach_support::TransientMessageInputPolicy::DismissAndForward
        };
        let delivered = if route_control_to_target_session {
            let attached = self
                .send_attached_overlay_to_session_identity(
                    &session_name,
                    display_session_id,
                    expanded.clone(),
                    duration,
                    input_policy,
                )
                .await;
            let control = self
                .send_display_message_to_control_session_identity(
                    &session_name,
                    display_session_id,
                    &expanded,
                )
                .await;
            attached || control
        } else {
            match display_client {
                Some(DisplayMessageClient::Attached(identity)) => {
                    self.send_attached_overlay_to_client_identity(
                        identity,
                        Some(display_session_id),
                        expanded.clone(),
                        duration,
                        input_policy,
                    )
                    .await
                }
                Some(DisplayMessageClient::Control(identity)) => {
                    self.send_control_response_notification_to_queue(
                        identity,
                        format_control_message_line(&expanded),
                    )
                    .await
                }
                None => false,
            }
        };
        if delivered {
            let mut state = self.state.lock().await;
            state.add_message(expanded);
        }

        Response::DisplayMessage(DisplayMessageResponse::no_output())
    }
}
