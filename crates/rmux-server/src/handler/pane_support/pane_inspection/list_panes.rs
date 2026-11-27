use std::path::Path;

use rmux_core::formats::{
    is_truthy, render_list_panes_line, FormatContext, DEFAULT_LIST_PANES_SESSION_FORMAT,
    DEFAULT_LIST_PANES_WINDOW_FORMAT,
};
use rmux_proto::{CommandOutput, ErrorResponse, ListPanesResponse, Response, RmuxError};

use super::super::super::RequestHandler;
#[cfg(windows)]
use super::super::pane_deferred_wait::format_references_pane_pid;
use super::list_panes_default::{
    push_default_list_panes_line, DefaultListPanesFormat, DefaultListPanesLineContext,
};
use crate::client_format::ClientLoopSnapshot;
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::{session_not_found, HandlerState};

impl RequestHandler {
    pub(in crate::handler) async fn handle_list_panes(
        &self,
        request: rmux_proto::ListPanesRequest,
    ) -> Response {
        let socket_path = self.socket_path();
        if let Some(response) = self.handle_internal_pane_exit_probe(&request).await {
            return response;
        }
        let attached_count = {
            let active_attach = self.active_attach.lock().await;
            active_attach.attached_count(&request.target)
        };
        let client_loop_snapshots = self.client_loop_snapshots().await;
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
            socket_path: &socket_path,
            attached_count,
            target_window_index: request.target_window_index,
            format: request.format.as_deref(),
            filter: request.filter.as_deref(),
            sort_order: request.sort_order.as_deref(),
            reversed: request.reversed,
            client_loop_snapshots: &client_loop_snapshots,
        }) {
            Ok(output) => output,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        Response::ListPanes(ListPanesResponse { output })
    }
}

struct ListPaneOutputSelection<'a> {
    state: &'a HandlerState,
    session: rmux_core::ManagedSessionView<'a>,
    socket_path: &'a Path,
    attached_count: usize,
    target_window_index: Option<u32>,
    format: Option<&'a str>,
    filter: Option<&'a str>,
    sort_order: Option<&'a str>,
    reversed: bool,
    client_loop_snapshots: &'a [ClientLoopSnapshot],
}

fn collect_list_pane_output_with_selection(
    selection: ListPaneOutputSelection<'_>,
) -> Result<CommandOutput, RmuxError> {
    let ListPaneOutputSelection {
        state,
        session,
        socket_path,
        attached_count,
        target_window_index,
        format,
        filter,
        sort_order,
        reversed,
        client_loop_snapshots,
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
    let session_context = FormatContext::new()
        .with_managed_session(session)
        .with_session_attached(attached_count);
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
        if target_window_index.is_some_and(|target| window_index != target) {
            continue;
        }

        let active = window_index == active_window;
        let last = Some(window_index) == last_window;
        let window_context =
            session_context
                .clone()
                .with_window(window_index, window, active, last);
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
                        window_index,
                        pane,
                        pane_active,
                    },
                ) {
                    continue;
                }
                stdout.pop();
            }
            let context = window_context.clone().with_pane(pane, pane_active);
            let runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_socket_path(socket_path)
                .with_session(session)
                .with_window(window_index, window)
                .with_pane(pane)
                .with_client_loop_snapshots(client_loop_snapshots);
            if let Some(filter) = filter {
                let expanded = render_runtime_template(filter, &runtime, false);
                if !is_truthy(&expanded) {
                    continue;
                }
            }
            if structured {
                window_rows.push(PaneListLine {
                    window_index,
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
pub(super) enum PaneListSortOrder {
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

pub(super) struct PaneListLine {
    pub(super) window_index: u32,
    pub(super) pane_index: u32,
    pub(super) size: rmux_core::PaneGeometry,
    pub(super) title: String,
    pub(super) created_at: i64,
    pub(super) active_point: u64,
    pub(super) rendered: String,
}

pub(super) fn sort_pane_list_lines(
    rows: &mut [PaneListLine],
    sort_order: PaneListSortOrder,
    reversed: bool,
) {
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
