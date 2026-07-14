use rmux_core::{
    formats::{
        is_truthy, render_list_panes_line, FormatContext, DEFAULT_DISPLAY_MESSAGE_FORMAT,
        DEFAULT_LIST_PANES_SESSION_FORMAT, DEFAULT_LIST_PANES_WINDOW_FORMAT,
    },
    PaneId,
};
use rmux_proto::{
    CommandOutput, DisplayMessageResponse, ErrorResponse, ListPanesResponse, Response, RmuxError,
    Target, TerminalSize,
};

use super::super::target_support::{pane_id_target, requester_environment_pane_id};
use super::super::{format_client_uid, format_client_user, ListClientSnapshot, RequestHandler};
#[cfg(windows)]
use super::pane_deferred_wait::format_references_pane_pid;
use crate::control_notifications::format_control_message_line;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};
use crate::renderer;

#[path = "inspection/list_panes_default.rs"]
mod list_panes_default;

use list_panes_default::{
    push_default_list_panes_line, DefaultListPanesFormat, DefaultListPanesLineContext,
};

struct DisplayMessageInvocation {
    target: Option<Target>,
    print: bool,
    message: Option<String>,
    target_client: Option<String>,
    expected_pane_id: Option<PaneId>,
    empty_target_context: bool,
    route_control_to_target_session: bool,
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_display_message(
        &self,
        requester_pid: u32,
        request: rmux_proto::DisplayMessageRequest,
    ) -> Response {
        self.handle_display_message_inner(
            requester_pid,
            DisplayMessageInvocation {
                target: request.target,
                print: request.print,
                message: request.message,
                target_client: None,
                expected_pane_id: None,
                empty_target_context: request.empty_target_context,
                route_control_to_target_session: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_display_message_ext(
        &self,
        requester_pid: u32,
        request: rmux_proto::DisplayMessageExtRequest,
    ) -> Response {
        self.handle_display_message_inner(
            requester_pid,
            DisplayMessageInvocation {
                target: request.target,
                print: request.print,
                message: request.message,
                target_client: request.target_client,
                expected_pane_id: None,
                empty_target_context: request.empty_target_context,
                route_control_to_target_session: false,
            },
        )
        .await
    }

    pub(in crate::handler) async fn handle_display_message_for_stable_pane(
        &self,
        requester_pid: u32,
        pane_id: PaneId,
        request: rmux_proto::DisplayMessageRequest,
    ) -> Response {
        let preferred_session = request.target.as_ref().map(Target::session_name).cloned();
        loop {
            let Some(target) = self
                .current_target_for_stable_pane(pane_id, preferred_session.as_ref())
                .await
            else {
                return Response::Error(ErrorResponse {
                    error: RmuxError::Server("target pane no longer exists".to_owned()),
                });
            };
            let response = self
                .handle_display_message_inner(
                    requester_pid,
                    DisplayMessageInvocation {
                        target: Some(Target::Pane(target)),
                        print: request.print,
                        message: request.message.clone(),
                        target_client: None,
                        expected_pane_id: Some(pane_id),
                        empty_target_context: request.empty_target_context,
                        route_control_to_target_session: true,
                    },
                )
                .await;
            if !display_message_stable_target_moved(&response) {
                return response;
            }
            tokio::task::yield_now().await;
        }
    }

    pub(in crate::handler) async fn current_target_for_stable_pane(
        &self,
        pane_id: PaneId,
        preferred_session: Option<&rmux_proto::SessionName>,
    ) -> Option<rmux_proto::PaneTarget> {
        let state = self.state.lock().await;
        preferred_session
            .and_then(|session_name| pane_target_for_id_in_session(&state, session_name, pane_id))
            .or_else(|| match pane_id_target(&state.sessions, pane_id.as_u32()) {
                Some(Target::Pane(target)) => Some(target),
                _ => None,
            })
    }

    async fn handle_display_message_inner(
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
        } = invocation;
        let target_attach_pid = match target_client.as_deref() {
            Some(target_client) => match self
                .find_target_attach_client_pid(requester_pid, target_client, "display-message")
                .await
            {
                Ok(Some(attach_pid)) => Some(attach_pid),
                Ok(None) if print => None,
                Ok(None) => {
                    return Response::DisplayMessage(DisplayMessageResponse::no_output());
                }
                Err(error) if print && display_message_client_is_control_only(&error) => None,
                Err(error) => return Response::Error(ErrorResponse { error }),
            },
            None => None,
        };
        let requester_is_control = self.is_control_client(requester_pid).await;
        let format_client_pid = match target_attach_pid {
            Some(attach_pid) => Some(attach_pid),
            None => self
                .resolve_target_attach_client_pid(requester_pid, None, "display-message")
                .await
                .ok(),
        };
        let requester_client = match format_client_pid {
            Some(attach_pid) => self
                .list_clients_snapshot()
                .await
                .into_iter()
                .find(|client| !client.control && client.pid == attach_pid),
            None => None,
        };
        let requester_environment_target = if target.is_none() && target_client.is_none() {
            let socket_path = self.socket_path();
            let requester_pane_id = requester_environment_pane_id(requester_pid, &socket_path);
            match requester_pane_id {
                Some(pane_id) => {
                    let state = self.state.lock().await;
                    pane_id_target(&state.sessions, pane_id)
                }
                None => None,
            }
        } else {
            None
        };
        let session_client_pid = target_attach_pid.unwrap_or(requester_pid);
        let attached_session_name = if target.is_none() && print {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .session_for_attached_client(session_client_pid, "display-message")
                .ok()
                .flatten()
        } else if target.is_none() {
            let active_attach = self.active_attach.lock().await;
            match active_attach.session_for_attached_client(session_client_pid, "display-message") {
                Ok(session_name) => session_name,
                Err(_error) if requester_is_control => None,
                Err(error) => return Response::Error(ErrorResponse { error }),
            }
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
                if let Some(client) = requester_client.as_ref() {
                    runtime = with_runtime_client_values(runtime, client);
                }
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
        let context_target =
            display_message_context_target(target, requester_environment_target, &session_name);
        #[cfg(windows)]
        if format_references_pane_pid(Some(template)) {
            self.wait_for_windows_deferred_target_pane_pids(&context_target)
                .await;
        }
        let attached_count = self.attached_count(&session_name).await;

        let (expanded, overlay_frame, clear_frame, duration) = {
            let mut state = self.state.lock().await;
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

            if print {
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

        if requester_is_control && target_attach_pid.is_none() && !route_control_to_target_session {
            self.send_control_notification_to(
                requester_pid,
                format_control_message_line(&expanded),
            )
            .await;
            return Response::DisplayMessage(DisplayMessageResponse::no_output());
        }

        let delivered = match target_attach_pid {
            Some(attach_pid) => {
                self.send_attached_overlay_to_client(
                    attach_pid,
                    overlay_frame,
                    clear_frame,
                    duration,
                )
                .await
            }
            None => {
                self.send_attached_overlay(&session_name, overlay_frame, clear_frame, duration)
                    .await
            }
        };
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
        #[cfg(windows)]
        if format_references_pane_pid(request.format.as_deref())
            || format_references_pane_pid(request.filter.as_deref())
        {
            self.wait_for_windows_deferred_list_pane_pids(
                &request.target,
                request.target_window_index,
            )
            .await;
        }
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

        let output = match collect_list_pane_output_with_selection(ListPaneOutputSelection {
            state: &state,
            session,
            attached_count,
            target_window_index: request.target_window_index,
            format: request.format.as_deref(),
            filter: request.filter.as_deref(),
            sort_order: request.sort_order.as_deref(),
            reversed: request.reversed,
        }) {
            Ok(output) => output,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        Response::ListPanes(ListPanesResponse { output })
    }
}

fn display_message_client_is_control_only(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Server(message) if message == "display-message requires an attached client"
    )
}

fn display_message_stable_target_moved(response: &Response) -> bool {
    matches!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(message),
        }) if message == "target pane changed before message delivery"
    )
}

fn pane_target_for_id_in_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Option<rmux_proto::PaneTarget> {
    let session = state.sessions.session(session_name)?;
    session.windows().iter().find_map(|(window_index, window)| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| {
                rmux_proto::PaneTarget::with_window(
                    session_name.clone(),
                    *window_index,
                    pane.index(),
                )
            })
    })
}

fn display_message_context_target(
    target: Option<Target>,
    requester_environment_target: Option<Target>,
    session_name: &rmux_proto::SessionName,
) -> Target {
    target
        .or(requester_environment_target)
        .unwrap_or_else(|| Target::Session(session_name.clone()))
}

fn target_resolves_to_pane_id(
    state: &HandlerState,
    target: &Target,
    expected_pane_id: PaneId,
) -> bool {
    let Target::Pane(target) = target else {
        return false;
    };
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.pane_id_in_window(target.window_index(), target.pane_index()))
        == Some(expected_pane_id)
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
        .with_named_value("client_key_table", client.key_table_name())
        .with_named_value("client_prefix", client.prefix_value())
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

struct ListPaneOutputSelection<'a> {
    state: &'a HandlerState,
    session: &'a rmux_core::Session,
    attached_count: usize,
    target_window_index: Option<u32>,
    format: Option<&'a str>,
    filter: Option<&'a str>,
    sort_order: Option<&'a str>,
    reversed: bool,
}

fn collect_list_pane_output_with_selection(
    selection: ListPaneOutputSelection<'_>,
) -> Result<CommandOutput, RmuxError> {
    let ListPaneOutputSelection {
        state,
        session,
        attached_count,
        target_window_index,
        format,
        filter,
        sort_order,
        reversed,
    } = selection;
    let sort_order = match PaneListSortOrder::parse(sort_order) {
        Some(sort_order) => sort_order,
        None if sort_order.is_some() => {
            return Err(RmuxError::Message(rmux_core::INVALID_SORT_ORDER.to_owned()));
        }
        None => PaneListSortOrder::Index,
    };
    let structured = filter.is_some() || sort_order.is_explicit();
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let session_context =
        FormatContext::from_session(session).with_session_attached(attached_count);
    let format = format.or(Some(if target_window_index.is_some() {
        DEFAULT_LIST_PANES_WINDOW_FORMAT
    } else {
        DEFAULT_LIST_PANES_SESSION_FORMAT
    }));
    let fast_format = if attached_count == 0 && !structured {
        format.and_then(DefaultListPanesFormat::from_format)
    } else {
        None
    };

    let mut stdout = Vec::new();
    let mut structured_lines = Vec::new();
    for (window_index, window) in session.windows() {
        if target_window_index.is_some_and(|target| *window_index != target) {
            continue;
        }

        let active = *window_index == active_window;
        let last = Some(*window_index) == last_window;
        let window_context =
            session_context
                .clone()
                .with_window(*window_index, window, active, last);
        let mut window_rows = Vec::new();

        for pane in window.panes() {
            let pane_active = pane.index() == window.active_pane_index();
            if let Some(fast_format) = fast_format {
                if !stdout.is_empty() {
                    stdout.push(b'\n');
                }
                if push_default_list_panes_line(
                    &mut stdout,
                    DefaultListPanesLineContext {
                        format: fast_format,
                        state,
                        session,
                        attached_count,
                        window_index: *window_index,
                        pane,
                        pane_active,
                    },
                ) {
                    continue;
                }
                stdout.pop();
            }
            let context = window_context.clone().with_pane(pane, pane_active);
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(*window_index, window)
                .with_pane(pane);
            if attached_count == 0 {
                runtime = runtime.with_unclipped_geometry();
            }
            if let Some(filter) = filter {
                let expanded = render_runtime_template(filter, &runtime, false);
                if !is_truthy(&expanded) {
                    continue;
                }
            }
            if structured {
                window_rows.push(PaneListLine {
                    window_index: *window_index,
                    pane_index: pane.index(),
                    size: pane.geometry(),
                    title: render_runtime_template("#{pane_title}", &runtime, false),
                    created_at: pane.created_at(),
                    active_point: pane.active_point(),
                    rendered: render_list_panes_line(&runtime, format),
                });
                continue;
            }
            if !stdout.is_empty() {
                stdout.push(b'\n');
            }
            stdout.extend_from_slice(render_list_panes_line(&runtime, format).as_bytes());
        }

        if structured {
            if sort_order.is_explicit() {
                sort_pane_list_lines(&mut window_rows, sort_order, reversed);
            }
            structured_lines.extend(window_rows.into_iter().map(|row| row.rendered));
        }
    }

    if structured {
        stdout = structured_lines.join("\n").into_bytes();
    }

    if !stdout.is_empty() {
        stdout.push(b'\n');
    }
    Ok(CommandOutput::from_stdout(stdout))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneListSortOrder {
    Index,
    ExplicitIndex,
    Name,
    Size,
    Activity,
    Creation,
}

impl PaneListSortOrder {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            None | Some("") => Some(Self::Index),
            Some("index" | "order") => Some(Self::ExplicitIndex),
            Some("name" | "title") => Some(Self::Name),
            Some("size") => Some(Self::Size),
            Some("activity") => Some(Self::Activity),
            Some("creation") => Some(Self::Creation),
            Some(_) => None,
        }
    }

    const fn is_explicit(self) -> bool {
        !matches!(self, Self::Index)
    }
}

struct PaneListLine {
    window_index: u32,
    pane_index: u32,
    size: rmux_core::PaneGeometry,
    title: String,
    created_at: i64,
    active_point: u64,
    rendered: String,
}

fn sort_pane_list_lines(rows: &mut [PaneListLine], sort_order: PaneListSortOrder, reversed: bool) {
    rows.sort_by(|left, right| {
        let primary = match sort_order {
            PaneListSortOrder::Index | PaneListSortOrder::ExplicitIndex => {
                (left.window_index, left.pane_index).cmp(&(right.window_index, right.pane_index))
            }
            PaneListSortOrder::Name => left.title.cmp(&right.title),
            PaneListSortOrder::Size => pane_area(left.size).cmp(&pane_area(right.size)),
            // tmux sorts pane "activity" by active_point (selection
            // counter, oracle-probed 2026-07-09): ascending, index-stable
            // until a pane is actually selected.
            PaneListSortOrder::Activity => left.active_point.cmp(&right.active_point),
            PaneListSortOrder::Creation => left.created_at.cmp(&right.created_at),
        };
        let primary = if reversed { primary.reverse() } else { primary };
        if matches!(sort_order, PaneListSortOrder::Size) {
            return primary;
        }
        primary.then_with(|| {
            (left.window_index, left.pane_index).cmp(&(right.window_index, right.pane_index))
        })
    });
}

fn pane_area(size: rmux_core::PaneGeometry) -> u64 {
    u64::from(size.cols()) * u64::from(size.rows())
}

pub(in crate::handler) fn command_output_from_lines(lines: &[String]) -> CommandOutput {
    if lines.is_empty() {
        return CommandOutput::from_stdout(Vec::new());
    }

    CommandOutput::from_stdout(format!("{}\n", lines.join("\n")).into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_core::PaneGeometry;
    use rmux_proto::{PaneTarget, SessionName};

    fn pane_list_line(pane_index: u32, cols: u16, rows: u16) -> PaneListLine {
        PaneListLine {
            window_index: 0,
            pane_index,
            size: PaneGeometry::new(0, 0, cols, rows),
            title: String::new(),
            created_at: 0,
            active_point: 0,
            rendered: pane_index.to_string(),
        }
    }

    #[test]
    fn pane_size_sort_uses_area_and_preserves_equal_area_order() {
        let original = vec![
            pane_list_line(9, 21, 10),
            pane_list_line(1, 10, 21),
            pane_list_line(7, 59, 5),
            pane_list_line(3, 20, 24),
        ];

        let mut ascending = original;
        sort_pane_list_lines(&mut ascending, PaneListSortOrder::Size, false);
        assert_eq!(
            ascending
                .iter()
                .map(|row| row.pane_index)
                .collect::<Vec<_>>(),
            [9, 1, 7, 3]
        );

        sort_pane_list_lines(&mut ascending, PaneListSortOrder::Size, true);
        assert_eq!(
            ascending
                .iter()
                .map(|row| row.pane_index)
                .collect::<Vec<_>>(),
            [3, 7, 9, 1]
        );
    }

    #[test]
    fn display_message_context_prefers_requester_pane_over_session_fallback() {
        let session = SessionName::new("beta").expect("session name");
        let requester_target = Target::Pane(PaneTarget::new(session.clone(), 3));

        assert_eq!(
            display_message_context_target(None, Some(requester_target.clone()), &session),
            requester_target
        );
    }

    #[test]
    fn display_message_context_keeps_explicit_target_over_requester_pane() {
        let fallback_session = SessionName::new("beta").expect("session name");
        let explicit_session = SessionName::new("alpha").expect("session name");
        let requester_target = Target::Pane(PaneTarget::new(fallback_session.clone(), 3));
        let explicit_target = Target::Session(explicit_session);

        assert_eq!(
            display_message_context_target(
                Some(explicit_target.clone()),
                Some(requester_target),
                &fallback_session,
            ),
            explicit_target
        );
    }
}
