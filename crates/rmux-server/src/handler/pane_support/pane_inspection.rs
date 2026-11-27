use rmux_core::{formats::FormatContext, PaneId};
use rmux_proto::{CommandOutput, ErrorResponse, Response, RmuxError, Target};

use super::super::target_support::pane_id_target;
use super::super::{ControlClientIdentity, ListClientSnapshot, RequestHandler};
use crate::format_runtime::RuntimeFormatContext;
use crate::handler::attach_support::ActiveAttachIdentity;
use crate::pane_terminals::{session_not_found, HandlerState};

mod display_message_clients;
mod display_message_dispatch;
mod list_panes;
mod list_panes_default;

#[cfg(test)]
use list_panes::{sort_pane_list_lines, PaneListLine, PaneListSortOrder};

struct DisplayMessageInvocation {
    target: Option<Target>,
    print: bool,
    message: Option<String>,
    target_client: Option<String>,
    expected_pane_id: Option<PaneId>,
    empty_target_context: bool,
    route_control_to_target_session: bool,
    duration_ms: Option<rmux_proto::DisplayMessageDurationMillis>,
    ignore_input: bool,
}

#[derive(Clone, Copy)]
enum DisplayMessageClient {
    Attached(ActiveAttachIdentity),
    Control(ControlClientIdentity),
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
                duration_ms: None,
                ignore_input: false,
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
                duration_ms: request.duration_ms,
                ignore_input: request.ignore_input,
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
                        route_control_to_target_session: !request.print,
                        duration_ms: None,
                        ignore_input: false,
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
    session.windows().find_map(|(window_index, window)| {
        window
            .panes()
            .iter()
            .find(|pane| pane.id() == pane_id)
            .map(|pane| {
                rmux_proto::PaneTarget::with_window(
                    session_name.clone(),
                    window_index,
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

/// The context of the pane display-message names, directly or as the active
/// pane of its target session or window: a poisoned transcript answers
/// "pane %N unavailable" rather than an empty expansion.
pub(in crate::handler) fn display_message_context<'a>(
    state: &'a HandlerState,
    target: &Target,
    attached_count: usize,
) -> Result<(rmux_core::ManagedSessionView<'a>, RuntimeFormatContext<'a>), RmuxError> {
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
            let mut context = FormatContext::new()
                .with_managed_session(session)
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
                runtime = runtime.with_named_pane(pane);
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
            let mut context = FormatContext::new()
                .with_managed_session(session)
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
                runtime = runtime.with_named_pane(pane);
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
            let context = FormatContext::new()
                .with_managed_session(session)
                .with_session_attached(attached_count)
                .with_window(
                    window_index,
                    window,
                    window_index == active_window,
                    Some(window_index) == last_window,
                )
                .with_pane(pane, pane_index == window.active_pane_index());
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_session(session)
                .with_window(window_index, window)
                .with_named_pane(pane);
            Ok((session, runtime))
        }
    }
}

fn with_runtime_client_values<'a>(
    runtime: RuntimeFormatContext<'a>,
    client: &ListClientSnapshot,
) -> RuntimeFormatContext<'a> {
    client.format_bindings().apply(runtime)
}

fn lone_session_name(sessions: &rmux_core::ManagedSessionStore) -> Option<rmux_proto::SessionName> {
    (sessions.len() == 1)
        .then(|| {
            sessions
                .iter()
                .map(|session| (session.name(), session))
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
            .unwrap_or(750),
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
