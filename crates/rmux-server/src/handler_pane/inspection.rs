use rmux_core::formats::{render_list_panes_line, FormatContext, DEFAULT_DISPLAY_MESSAGE_FORMAT};
use rmux_proto::{
    CommandOutput, DisplayMessageResponse, ErrorResponse, ListPanesResponse, Response, RmuxError,
    Target, TerminalSize,
};

use super::super::{format_client_uid, format_client_user, ListClientSnapshot, RequestHandler};
use crate::control_notifications::format_control_message_line;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::renderer;

impl RequestHandler {
    pub(in crate::handler) async fn handle_display_message(
        &self,
        requester_pid: u32,
        request: rmux_proto::DisplayMessageRequest,
    ) -> Response {
        let target = request.target;
        let requester_is_control = self.is_control_client(requester_pid).await;
        let requester_client = match self
            .resolve_target_attach_client_pid(requester_pid, None, "display-message")
            .await
        {
            Ok(attach_pid) => self
                .list_clients_snapshot()
                .await
                .into_iter()
                .find(|client| !client.control && client.pid == attach_pid),
            Err(_) => None,
        };
        let attached_session_name = if target.is_none() && request.print {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .session_for_attached_client(requester_pid, "display-message")
                .ok()
                .flatten()
        } else if target.is_none() {
            let active_attach = self.active_attach.lock().await;
            match active_attach.session_for_attached_client(requester_pid, "display-message") {
                Ok(session_name) => session_name,
                Err(_error) if requester_is_control => None,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
        } else {
            None
        };
        let fallback_session_name = if attached_session_name.is_some() {
            attached_session_name
        } else if requester_is_control {
            self.control_session_name(requester_pid).await
        } else {
            None
        };

        if target.is_none()
            && fallback_session_name.is_none()
            && !request.print
            && !requester_is_control
        {
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        }

        let mut session_name = target
            .as_ref()
            .map(|target| target.session_name().clone())
            .or(fallback_session_name);
        let template = request
            .message
            .as_deref()
            .unwrap_or(DEFAULT_DISPLAY_MESSAGE_FORMAT);
        let mut uses_lone_session_print_context = false;

        if request.print && session_name.is_none() {
            session_name = {
                let state = self.state.lock().await;
                lone_session_name(&state.sessions)
            };
            uses_lone_session_print_context = session_name.is_some();
        }
        if request.print && session_name.is_none() {
            session_name = self.preferred_session_name().await.ok();
        }

        if request.print && session_name.is_none() {
            let expanded = {
                let state = self.state.lock().await;
                let mut runtime =
                    RuntimeFormatContext::new(FormatContext::new()).with_state(&state);
                if let Some(client) = requester_client.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
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
                if let Some(client) = requester_client.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
                render_runtime_template(template, &runtime, true)
            };
            self.send_control_notification_to(
                requester_pid,
                format_control_message_line(&expanded),
            )
            .await;
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        };
        let context_target = target.unwrap_or_else(|| Target::Session(session_name.clone()));
        let attached_count = self.attached_count(&session_name).await;

        let (expanded, overlay_frame, clear_frame, duration) = {
            let mut state = self.state.lock().await;
            if let Err(error) = state.refresh_format_target_exit_status(&context_target) {
                return Response::Error(ErrorResponse { error });
            }
            let (session, mut context) =
                match display_message_context(&state, &context_target, attached_count) {
                    Ok(context) => context,
                    Err(error) => return Response::Error(ErrorResponse { error }),
                };
            if let Some(client) = requester_client.as_ref() {
                context = with_runtime_client_values(context, client);
            }
            if uses_lone_session_print_context {
                context = context.without_session_size();
                if requester_client.is_none() {
                    context = context.with_unclipped_geometry();
                }
            }
            context = context.with_named_value(
                "socket_path",
                self.socket_path().to_string_lossy().into_owned(),
            );
            let expanded = render_runtime_template(template, &context, true);

            if request.print {
                return Response::DisplayMessage(DisplayMessageResponse::from_output(
                    CommandOutput::from_stdout(format!("{expanded}\n").into_bytes()),
                ));
            }

            let mut overlay_frame = renderer::render_display_panes_clear(session, &state.options);
            overlay_frame.extend_from_slice(
                renderer::render_status_message(session, &state.options, &expanded).as_slice(),
            );
            let clear_frame = renderer::render_display_panes_clear(session, &state.options);
            (
                expanded,
                overlay_frame,
                clear_frame,
                display_time(&state.options, &session_name),
            )
        };

        if requester_is_control {
            self.send_control_notification_to(
                requester_pid,
                format_control_message_line(&expanded),
            )
            .await;
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        }

        let delivered = self
            .send_attached_overlay(&session_name, overlay_frame, clear_frame, duration)
            .await;
        if delivered {
            let mut state = self.state.lock().await;
            state.add_message(expanded);
        }

        Response::DisplayMessage(DisplayMessageResponse::no_output())
    }

    pub(in crate::handler) async fn handle_list_panes(
        &self,
        request: rmux_proto::ListPanesRequest,
    ) -> Response {
        let attached_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(&request.target)
        };
        let mut state = self.state.lock().await;
        if let Err(error) =
            state.refresh_list_panes_exit_statuses(&request.target, request.target_window_index)
        {
            return Response::Error(ErrorResponse { error });
        }
        let Some(session) = state.sessions.session(&request.target) else {
            return Response::Error(ErrorResponse {
                error: session_not_found(&request.target),
            });
        };
        if let Some(window_index) = request.target_window_index {
            if session.window_at(window_index).is_none() {
                return Response::Error(ErrorResponse {
                    error: RmuxError::invalid_target(
                        format!("{}:{window_index}", request.target),
                        "window index does not exist in session",
                    ),
                });
            }
        }

        Response::ListPanes(ListPanesResponse {
            output: command_output_from_lines(&collect_list_pane_lines(
                &state,
                session,
                attached_count,
                request.target_window_index,
                request.format.as_deref(),
            )),
        })
    }
}

pub(in crate::handler) fn display_message_context<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
) -> Result<(&'a rmux_core::Session, RuntimeFormatContext<'a>), RmuxError> {
    let session_name = target.session_name();
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();

    match target {
        Target::Session(_) => {
            let window = session.window();
            let use_unclipped_geometry = attached_count == 0 && window.pane_count() == 1;
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(active_window, window, true, false);
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(active_window, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            if use_unclipped_geometry {
                runtime = runtime.with_unclipped_geometry();
            }
            Ok((session, runtime))
        }
        Target::Window(target) => {
            let window_index = target.window_index();
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            let use_unclipped_geometry = attached_count == 0 && window.pane_count() == 1;
            let mut context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                );
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            if use_unclipped_geometry {
                runtime = runtime.with_unclipped_geometry();
            }
            Ok((session, runtime))
        }
        Target::Pane(target) => {
            let window_index = target.window_index();
            let pane_index = target.pane_index();
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{}:{window_index}", target.session_name()),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(pane_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                )
            })?;
            let use_unclipped_geometry = attached_count == 0 && window.pane_count() == 1;
            let context = FormatContext::from_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                )
                .with_pane(pane, pane_index == window.active_pane_index());
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window)
                .with_pane(pane);
            if use_unclipped_geometry {
                runtime = runtime.with_unclipped_geometry();
            }
            Ok((session, runtime))
        }
    }
}

fn with_runtime_client_values<'a>(
    runtime: RuntimeFormatContext<'a>,
    client: &ListClientSnapshot,
) -> RuntimeFormatContext<'a> {
    runtime
        .with_client_size(TerminalSize {
            cols: client.width,
            rows: client.height,
        })
        .with_named_value("client_name", client.name.clone())
        .with_named_value("client_pid", client.pid.to_string())
        .with_named_value("client_tty", client.tty.clone())
        .with_named_value("client_width", client.width.to_string())
        .with_named_value("client_height", client.height.to_string())
        .with_named_value("client_termfeatures", client.termfeatures.clone())
        .with_named_value("client_termname", client.termname.clone())
        .with_named_value("client_termtype", client.termtype.clone())
        .with_named_value("client_uid", format_client_uid(client.uid))
        .with_named_value("client_user", format_client_user(client.uid, &client.user))
        .with_named_value("client_utf8", if client.utf8 { "1" } else { "0" })
        .with_named_value(
            "client_control_mode",
            if client.control { "1" } else { "0" },
        )
        .with_named_value("client_flags", client.flags.clone())
}

fn lone_session_name(sessions: &rmux_core::SessionStore) -> Option<rmux_proto::SessionName> {
    (sessions.len() == 1)
        .then(|| {
            sessions
                .iter()
                .next()
                .map(|(session_name, _)| session_name.clone())
        })
        .flatten()
}

pub(in crate::handler) fn display_time(
    options: &rmux_core::OptionStore,
    session_name: &rmux_proto::SessionName,
) -> std::time::Duration {
    std::time::Duration::from_millis(
        options
            .resolve(Some(session_name), rmux_proto::OptionName::DisplayTime)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(750)
            .max(1),
    )
}

pub(in crate::handler) fn attached_status_message_for_error(error: &RmuxError) -> String {
    let message = error.to_string();
    match message.as_str() {
        // tmux keeps these errors lower-case for detached commands, but renders
        // them sentence-cased in the attached status row.
        "no next window" => "No next window".to_owned(),
        "no previous window" => "No previous window".to_owned(),
        "no space for new pane" => "No space for new pane".to_owned(),
        _ => message,
    }
}

fn collect_list_pane_lines(
    state: &HandlerState,
    session: &rmux_core::Session,
    attached_count: usize,
    target_window_index: Option<u32>,
    format: Option<&str>,
) -> Vec<String> {
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let session_context =
        FormatContext::from_session(session).with_session_attached(attached_count);

    session
        .windows()
        .iter()
        .filter(|(window_index, _)| {
            target_window_index
                .map(|target| **window_index == target)
                .unwrap_or(true)
        })
        .flat_map(|(window_index, window)| {
            let active = *window_index == active_window;
            let last = Some(*window_index) == last_window;
            let window_context =
                session_context
                    .clone()
                    .with_window(*window_index, window, active, last);

            window.panes().iter().map(move |pane| {
                let context = window_context
                    .clone()
                    .with_pane(pane, pane.index() == window.active_pane_index());
                let mut runtime = RuntimeFormatContext::new(context)
                    .with_state(state)
                    .with_session(session)
                    .with_window(*window_index, window)
                    .with_pane(pane);
                if attached_count == 0 {
                    runtime = runtime.with_unclipped_geometry();
                }
                render_list_panes_line(&runtime, format)
            })
        })
        .collect()
}

pub(in crate::handler) fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        return CommandOutput::from_stdout(Vec::new());
    }

    CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
}
