use std::cmp::Ordering;
use std::path::Path;

use rmux_client::connect;
use rmux_core::formats::{DEFAULT_LIST_WINDOWS_ALL_FORMAT, DEFAULT_LIST_WINDOWS_FORMAT};
use rmux_proto::{
    CommandOutput, ErrorResponse, Response, WindowListEntry, CAPABILITY_CLI_LIST_WINDOWS_ALL_QUEUE,
};

use super::super::command_runner::{
    capture_list_windows_all_server_command_with_connection,
    run_list_windows_all_server_command_with_connection,
};
use super::super::json_output::{list_windows_json_format, write_list_windows_json};
use super::super::{
    list_session_names, resolve_session_listing_target, unexpected_response, write_lines_output,
    ExitFailure,
};
use super::LIST_WINDOWS_FILTER_SEPARATOR;
use crate::cli_args::ListWindowsArgs;

const LIST_WINDOWS_SORT_METADATA_FORMAT: &str = "#{window_activity}\x1f#{window_created}";

pub(in crate::cli) fn run_list_windows(
    args: ListWindowsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let json = args.json;
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let queued_all_sessions = args.all_sessions
        && connection
            .supports_capability(CAPABILITY_CLI_LIST_WINDOWS_ALL_QUEUE)
            .map_err(ExitFailure::from_client)?;
    if queued_all_sessions {
        let json_format = json.then(list_windows_json_format);
        let arguments = list_windows_all_queue_arguments(&args, json_format.as_deref());
        if json {
            let (exit_status, output) = capture_list_windows_all_server_command_with_connection(
                &mut connection,
                &arguments,
            )?;
            if exit_status != 0 {
                return Ok(exit_status);
            }
            return write_list_windows_json(&output);
        }
        return run_list_windows_all_server_command_with_connection(&mut connection, &arguments);
    }
    let targets = if args.all_sessions {
        list_session_names(&mut connection)?
    } else {
        vec![resolve_session_listing_target(
            &mut connection,
            args.target,
            "list-windows",
        )?]
    };
    let sort_order = CliWindowListSortOrder::parse(args.sort_order.as_deref())?;
    let include_sort_metadata = args.all_sessions
        && matches!(
            sort_order,
            CliWindowListSortOrder::Activity | CliWindowListSortOrder::Creation
        );
    let json_format = json.then(list_windows_json_format);
    let format = list_windows_server_format(
        json_format.as_deref().or(args.format.as_deref()),
        args.all_sessions,
        include_sort_metadata,
    );
    let mut lines = Vec::new();
    let mut windows = Vec::new();
    for target in targets {
        let target_sort_order = if args.all_sessions {
            None
        } else {
            args.sort_order.clone()
        };
        let target_reversed = !args.all_sessions && args.reversed;
        let response = connection
            .list_windows_with_options(
                target,
                format.clone(),
                args.filter.clone(),
                target_sort_order,
                target_reversed,
            )
            .map_err(ExitFailure::from_client)?;
        match response {
            Response::ListWindows(response) => {
                let parsed_windows = response
                    .windows
                    .into_iter()
                    .map(|mut window| {
                        // The RPC filters on the complete value before rendering this row.
                        let (activity_at, created_at, rendered) =
                            list_windows_sort_metadata(&window.rendered, include_sort_metadata)?;
                        window.rendered = rendered.to_owned();
                        Ok(CliWindowListEntry {
                            window,
                            activity_at,
                            created_at,
                        })
                    })
                    .collect::<Result<Vec<_>, ExitFailure>>()?;
                if json || args.all_sessions {
                    windows.extend(parsed_windows);
                } else {
                    lines.extend(
                        parsed_windows
                            .into_iter()
                            .map(|window| window.window.rendered),
                    );
                }
            }
            Response::Error(ErrorResponse { error }) => {
                return Err(ExitFailure::new(1, error.to_string()))
            }
            other => return Err(unexpected_response("list-windows", &other)),
        }
    }
    if args.all_sessions {
        sort_all_session_window_entries(&mut windows, sort_order, args.reversed);
        if !json {
            lines.extend(windows.iter().map(|window| window.window.rendered.clone()));
        }
    }
    if json {
        let mut output = Vec::new();
        for entry in windows {
            output.extend_from_slice(entry.window.rendered.as_bytes());
            output.push(b'\n');
        }
        return write_list_windows_json(&CommandOutput::from_stdout(output));
    }
    write_lines_output(&lines)
}

fn list_windows_all_queue_arguments(
    args: &ListWindowsArgs,
    format_override: Option<&str>,
) -> Vec<String> {
    rmux_core::cli_entry::list_windows_all_queue_arguments(
        args.target.as_ref().map(|target| target.raw()),
        format_override.or(args.format.as_deref()),
        args.filter.as_deref(),
        args.sort_order.as_deref(),
        args.reversed,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliWindowListSortOrder {
    Index,
    Name,
    Size,
    Activity,
    Creation,
    ExplicitIndex,
}

impl CliWindowListSortOrder {
    fn parse(value: Option<&str>) -> Result<Self, ExitFailure> {
        match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
            None | Some("") => Ok(Self::Index),
            Some("index" | "order") => Ok(Self::ExplicitIndex),
            Some("name" | "title") => Ok(Self::Name),
            Some("size") => Ok(Self::Size),
            Some("activity") => Ok(Self::Activity),
            Some("creation") => Ok(Self::Creation),
            Some(_) => Err(ExitFailure::new(
                1,
                rmux_core::INVALID_SORT_ORDER.to_owned(),
            )),
        }
    }

    const fn is_explicit(self) -> bool {
        !matches!(self, Self::Index)
    }
}

struct CliWindowListEntry {
    window: WindowListEntry,
    activity_at: Option<i64>,
    created_at: Option<i64>,
}

fn sort_all_session_window_entries(
    windows: &mut [CliWindowListEntry],
    sort_order: CliWindowListSortOrder,
    reversed: bool,
) {
    if !sort_order.is_explicit() {
        return;
    }
    windows.sort_by(|left, right| {
        let primary = match sort_order {
            CliWindowListSortOrder::Index | CliWindowListSortOrder::ExplicitIndex => left
                .window
                .target
                .window_index()
                .cmp(&right.window.target.window_index()),
            CliWindowListSortOrder::Name => stable_window_entry_name_cmp(left, right),
            CliWindowListSortOrder::Size => {
                cli_terminal_area(left.window.size).cmp(&cli_terminal_area(right.window.size))
            }
            CliWindowListSortOrder::Activity => right.activity_at.cmp(&left.activity_at),
            CliWindowListSortOrder::Creation => left.created_at.cmp(&right.created_at),
        };
        let primary = if reversed { primary.reverse() } else { primary };
        primary
            .then_with(|| {
                left.window
                    .target
                    .session_name()
                    .as_str()
                    .cmp(right.window.target.session_name().as_str())
            })
            .then_with(|| {
                left.window
                    .target
                    .window_index()
                    .cmp(&right.window.target.window_index())
            })
            .then_with(|| stable_window_entry_name_cmp(left, right))
    });
}

fn cli_terminal_area(size: rmux_proto::TerminalSize) -> u64 {
    u64::from(size.cols) * u64::from(size.rows)
}

fn stable_window_entry_name_cmp(left: &CliWindowListEntry, right: &CliWindowListEntry) -> Ordering {
    left.window.name.cmp(&right.window.name)
}

fn list_windows_server_format(
    format: Option<&str>,
    all_sessions: bool,
    include_sort_metadata: bool,
) -> Option<String> {
    let default_format = if all_sessions {
        DEFAULT_LIST_WINDOWS_ALL_FORMAT
    } else {
        DEFAULT_LIST_WINDOWS_FORMAT
    };
    let mut line_format = format
        .map(ToOwned::to_owned)
        .or_else(|| all_sessions.then(|| default_format.to_owned()));
    if include_sort_metadata {
        let rendered = line_format.as_deref().unwrap_or(default_format);
        line_format = Some(format!(
            "{LIST_WINDOWS_SORT_METADATA_FORMAT}{LIST_WINDOWS_FILTER_SEPARATOR}{rendered}"
        ));
    }
    line_format
}

fn list_windows_sort_metadata(
    line: &str,
    include_sort_metadata: bool,
) -> Result<(Option<i64>, Option<i64>, &str), ExitFailure> {
    if !include_sort_metadata {
        return Ok((None, None, line));
    }

    let Some((activity, rest)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
        return Err(ExitFailure::new(
            1,
            "list-windows sort metadata missing activity separator",
        ));
    };
    let Some((created, rendered)) = rest.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
        return Err(ExitFailure::new(
            1,
            "list-windows sort metadata missing creation separator",
        ));
    };
    let activity_at = activity.parse::<i64>().map_err(|_| {
        ExitFailure::new(1, "list-windows sort metadata has invalid activity value")
    })?;
    let created_at = created.parse::<i64>().map_err(|_| {
        ExitFailure::new(1, "list-windows sort metadata has invalid creation value")
    })?;
    Ok((Some(activity_at), Some(created_at), rendered))
}
