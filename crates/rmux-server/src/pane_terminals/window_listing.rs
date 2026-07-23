use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::Path;

use rmux_core::{
    formats::{
        is_truthy, render_list_windows_line, FormatContext, DEFAULT_LIST_WINDOWS_ALL_FORMAT,
    },
    Session,
};
use rmux_proto::{
    CommandOutput, ListWindowsResponse, RmuxError, SessionName, WindowListEntry, WindowTarget,
};

use super::{session_not_found, HandlerState};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};

pub(crate) struct ListWindowsSelection<'a> {
    pub(crate) session_name: &'a SessionName,
    pub(crate) socket_path: &'a Path,
    pub(crate) format: Option<&'a str>,
    pub(crate) attached_count: usize,
    pub(crate) filter: Option<&'a str>,
    pub(crate) sort_order: Option<&'a str>,
    pub(crate) reversed: bool,
}

pub(crate) struct ListWindowsAllSelection<'a> {
    pub(crate) socket_path: &'a Path,
    pub(crate) format: Option<&'a str>,
    pub(crate) attached_counts: &'a HashMap<SessionName, usize>,
    pub(crate) filter: Option<&'a str>,
    pub(crate) sort_order: Option<&'a str>,
    pub(crate) reversed: bool,
}

impl HandlerState {
    pub(crate) fn list_windows(
        &self,
        selection: ListWindowsSelection<'_>,
    ) -> Result<ListWindowsResponse, RmuxError> {
        let ListWindowsSelection {
            session_name,
            socket_path,
            format,
            attached_count,
            filter,
            sort_order,
            reversed,
        } = selection;
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let sort_order = match WindowListSortOrder::parse(sort_order) {
            Some(sort_order) => sort_order,
            None if sort_order.is_some() => {
                return Err(RmuxError::Message(rmux_core::INVALID_SORT_ORDER.to_owned()));
            }
            None => WindowListSortOrder::Index,
        };
        let mut rows = collect_window_entries(
            self,
            session,
            session_name,
            socket_path,
            format,
            attached_count,
            filter,
        );
        if sort_order != WindowListSortOrder::Index || reversed && sort_order.is_explicit() {
            sort_window_entries(&mut rows, sort_order, reversed);
        }
        Ok(list_windows_response(rows))
    }

    pub(crate) fn list_windows_all(
        &self,
        selection: ListWindowsAllSelection<'_>,
    ) -> Result<ListWindowsResponse, RmuxError> {
        let ListWindowsAllSelection {
            socket_path,
            format,
            attached_counts,
            filter,
            sort_order,
            reversed,
        } = selection;
        let sort_order = match WindowListSortOrder::parse(sort_order) {
            Some(sort_order) => sort_order,
            None => return Err(RmuxError::Message(rmux_core::INVALID_SORT_ORDER.to_owned())),
        };
        let format = Some(format.unwrap_or(DEFAULT_LIST_WINDOWS_ALL_FORMAT));
        let mut session_names = self
            .sessions
            .iter()
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        session_names.sort_by(|left, right| left.as_str().cmp(right.as_str()));

        let mut rows = Vec::new();
        for session_name in session_names {
            let session = self
                .sessions
                .session(&session_name)
                .expect("listed session remains locked");
            rows.extend(collect_window_entries(
                self,
                session,
                &session_name,
                socket_path,
                format,
                attached_counts.get(&session_name).copied().unwrap_or(0),
                filter,
            ));
        }
        if sort_order.is_explicit() {
            sort_all_window_entries(&mut rows, sort_order, reversed);
        }
        Ok(list_windows_response(rows))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowListSortOrder {
    Index,
    Name,
    Size,
    Activity,
    Creation,
    ExplicitIndex,
}

impl WindowListSortOrder {
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

struct WindowListRow {
    entry: WindowListEntry,
    created_at: i64,
    activity_at: i64,
}

fn collect_window_entries(
    state: &HandlerState,
    session: &Session,
    session_name: &SessionName,
    socket_path: &Path,
    format: Option<&str>,
    attached_count: usize,
    filter: Option<&str>,
) -> Vec<WindowListRow> {
    let active_window = session.active_window_index();
    let last_window = session.last_window_index();
    let session_context =
        FormatContext::from_session(session).with_session_attached(attached_count);

    session
        .windows()
        .iter()
        .filter_map(|(window_index, window)| {
            let active = *window_index == active_window;
            let last = Some(*window_index) == last_window;
            let mut context =
                session_context
                    .clone()
                    .with_window(*window_index, window, active, last);
            if let Some(pane) = window.active_pane() {
                context = context.with_window_pane(window, pane);
            }
            let mut runtime = RuntimeFormatContext::new(context)
                .with_state(state)
                .with_socket_path(socket_path)
                .with_session(session)
                .with_window(*window_index, window);
            if let Some(pane) = window.active_pane() {
                runtime = runtime.with_pane(pane);
            }
            if attached_count == 0 {
                runtime = runtime.with_unclipped_geometry();
            }
            if let Some(filter) = filter {
                let expanded = render_runtime_template(filter, &runtime, false);
                if !is_truthy(&expanded) {
                    return None;
                }
            }
            let rendered_name = render_runtime_template("#{window_name}", &runtime, false);
            let rendered = render_list_windows_line(&runtime, format);

            Some(WindowListRow {
                entry: WindowListEntry {
                    target: WindowTarget::with_window(session_name.clone(), *window_index),
                    window_id: window.id().to_string(),
                    name: (!rendered_name.is_empty()).then_some(rendered_name),
                    pane_count: u32::try_from(window.pane_count()).expect("pane count fits in u32"),
                    size: window.size(),
                    layout: window.layout(),
                    active,
                    last,
                    rendered,
                },
                created_at: window.created_at(),
                activity_at: window.activity_at(),
            })
        })
        .collect()
}

fn sort_window_entries(
    entries: &mut [WindowListRow],
    sort_order: WindowListSortOrder,
    reversed: bool,
) {
    entries.sort_by(|left, right| {
        let primary = compare_window_entries(left, right, sort_order);
        let primary = if reversed { primary.reverse() } else { primary };
        if matches!(sort_order, WindowListSortOrder::Size) {
            return primary;
        }
        primary
            .then_with(|| stable_window_name_cmp(left, right))
            .then_with(|| {
                left.entry
                    .target
                    .window_index()
                    .cmp(&right.entry.target.window_index())
            })
    });
}

fn sort_all_window_entries(
    entries: &mut [WindowListRow],
    sort_order: WindowListSortOrder,
    reversed: bool,
) {
    entries.sort_by(|left, right| {
        let primary = compare_window_entries(left, right, sort_order);
        let primary = if reversed { primary.reverse() } else { primary };
        primary
            .then_with(|| {
                left.entry
                    .target
                    .session_name()
                    .as_str()
                    .cmp(right.entry.target.session_name().as_str())
            })
            .then_with(|| {
                left.entry
                    .target
                    .window_index()
                    .cmp(&right.entry.target.window_index())
            })
            .then_with(|| stable_window_name_cmp(left, right))
    });
}

fn compare_window_entries(
    left: &WindowListRow,
    right: &WindowListRow,
    sort_order: WindowListSortOrder,
) -> Ordering {
    match sort_order {
        WindowListSortOrder::Index | WindowListSortOrder::ExplicitIndex => left
            .entry
            .target
            .window_index()
            .cmp(&right.entry.target.window_index()),
        WindowListSortOrder::Name => stable_window_name_cmp(left, right),
        WindowListSortOrder::Size => {
            terminal_area(left.entry.size).cmp(&terminal_area(right.entry.size))
        }
        WindowListSortOrder::Activity => right.activity_at.cmp(&left.activity_at),
        WindowListSortOrder::Creation => left.created_at.cmp(&right.created_at),
    }
}

fn terminal_area(size: rmux_proto::TerminalSize) -> u64 {
    u64::from(size.cols) * u64::from(size.rows)
}

fn stable_window_name_cmp(left: &WindowListRow, right: &WindowListRow) -> Ordering {
    left.entry.name.cmp(&right.entry.name)
}

fn list_windows_response(rows: Vec<WindowListRow>) -> ListWindowsResponse {
    let windows = rows.into_iter().map(|row| row.entry).collect::<Vec<_>>();
    let output = build_command_output(&windows);
    ListWindowsResponse { windows, output }
}

fn build_command_output(windows: &[WindowListEntry]) -> CommandOutput {
    let stdout = windows
        .iter()
        .map(|window| window.rendered.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = if stdout.is_empty() {
        Vec::new()
    } else {
        format!("{stdout}\n").into_bytes()
    };
    CommandOutput::from_stdout(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{LayoutName, TerminalSize};

    fn window_list_row(window_index: u32, name: &str, cols: u16, rows: u16) -> WindowListRow {
        WindowListRow {
            entry: WindowListEntry {
                target: WindowTarget::with_window(
                    SessionName::new("sort-area").expect("valid session name"),
                    window_index,
                ),
                window_id: format!("@{window_index}"),
                name: Some(name.to_owned()),
                pane_count: 1,
                size: TerminalSize { cols, rows },
                layout: LayoutName::Tiled,
                active: false,
                last: false,
                rendered: window_index.to_string(),
            },
            created_at: 0,
            activity_at: 0,
        }
    }

    #[test]
    fn window_size_sort_uses_area_and_preserves_equal_area_order() {
        let rows = || {
            vec![
                window_list_row(9, "z", 21, 10),
                window_list_row(1, "a", 10, 21),
                window_list_row(7, "middle", 59, 5),
                window_list_row(3, "large", 20, 24),
            ]
        };

        let mut ascending = rows();
        sort_window_entries(&mut ascending, WindowListSortOrder::Size, false);
        assert_eq!(
            ascending
                .iter()
                .map(|row| row.entry.target.window_index())
                .collect::<Vec<_>>(),
            [9, 1, 7, 3]
        );

        let mut descending = rows();
        sort_window_entries(&mut descending, WindowListSortOrder::Size, true);
        assert_eq!(
            descending
                .iter()
                .map(|row| row.entry.target.window_index())
                .collect::<Vec<_>>(),
            [3, 7, 9, 1]
        );
    }
}
