use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_core::formats::{
    is_truthy, DEFAULT_LIST_PANES_ALL_FORMAT, DEFAULT_LIST_PANES_SESSION_FORMAT,
    DEFAULT_LIST_PANES_WINDOW_FORMAT,
};
use rmux_proto::{
    CommandOutput, ResizePaneAdjustment, ResizePaneRelativeDirection,
    ResizePaneTargetActionRequest, ResolveTargetType, RespawnPaneRequest,
};

#[path = "pane_commands/split.rs"]
mod split;
#[path = "pane_commands/transfer.rs"]
mod transfer;

use super::json_output::{
    filter_delimited_json_output, list_panes_json_format, write_list_panes_json,
};
use super::{
    cli_target_actions_enabled, expect_command_output, expect_command_success, list_session_names,
    resolve_current_pane_target, resolve_pane_target_or_current, resolve_pane_target_spec,
    resolve_session_listing_target, resolve_target_spec, resolve_window_target_or_current,
    run_command_resolved, shell_command_text, target_action_needs_legacy_retry, write_lines_output,
    ExitFailure,
};
use crate::cli_args::{
    LastPaneArgs, ListPanesArgs, PipePaneArgs, ResizePaneArgs, RespawnPaneArgs, SelectPaneArgs,
    TargetSpec,
};

pub(super) use split::run_split_window;
pub(super) use transfer::{run_break_pane, run_join_pane, run_move_pane, run_swap_pane};

const LIST_PANES_FILTER_SEPARATOR: char = '\x1f';

pub(super) fn run_last_pane(args: LastPaneArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "last-pane", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "last-pane")?;
        let input_disabled = if args.enable_input {
            Some(false)
        } else if args.disable_input {
            Some(true)
        } else {
            None
        };
        connection
            .last_pane_with_options(target, args.keep_zoom, input_disabled)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_pipe_pane(args: PipePaneArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let command = (!args.command.is_empty()).then(|| shell_command_text(args.command));
    let stdout = if !args.stdin && !args.stdout {
        true
    } else {
        args.stdout
    };
    run_command_resolved(socket_path, "pipe-pane", move |connection| {
        let target = resolve_pane_target_or_current(connection, args.target.as_ref(), "pipe-pane")?;
        connection
            .pipe_pane(target, args.stdin, stdout, args.once, command)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_respawn_pane(
    args: RespawnPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "respawn-pane", move |connection| {
        let target =
            resolve_pane_target_or_current(connection, args.target.as_ref(), "respawn-pane")?;
        connection
            .respawn_pane(RespawnPaneRequest {
                target,
                kill: args.kill,
                start_directory: args.start_directory,
                environment: (!args.environment.is_empty()).then_some(args.environment),
                command: (!args.command.is_empty()).then_some(args.command),
                process_command: None,
            })
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_list_panes(args: ListPanesArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let json = args.json;
    let default_format = list_panes_default_format(args.all_sessions, args.session_scope);
    let format = if json {
        Some(list_panes_server_format(
            Some(&list_panes_json_format()),
            args.filter.as_deref(),
            default_format,
        ))
    } else {
        Some(list_panes_server_format(
            args.format.as_deref(),
            args.filter.as_deref(),
            default_format,
        ))
    };
    let pane_targets = if args.all_sessions {
        list_session_names(&mut connection)?
            .into_iter()
            .map(|session_name| (session_name, None))
            .collect::<Vec<_>>()
    } else {
        let (session_name, target_window_index) =
            resolve_list_panes_target(&mut connection, args.target, "list-panes")?;
        vec![(
            session_name,
            if args.session_scope {
                None
            } else {
                target_window_index
            },
        )]
    };
    let mut lines = Vec::new();
    let mut json_stdout = Vec::new();
    for (session_name, target_window_index) in pane_targets {
        let response = connection
            .list_panes_in_window_with_options(
                session_name.clone(),
                target_window_index,
                format.clone(),
                args.filter.clone(),
                args.sort_order.clone(),
                args.reversed,
            )
            .map_err(ExitFailure::from_client)?;
        let output = expect_command_output(&response, "list-panes")?;
        if json {
            let filtered;
            let output = if args.filter.is_some() {
                filtered = filter_delimited_json_output(output, "list-panes")?;
                &filtered
            } else {
                output
            };
            json_stdout.extend_from_slice(output.stdout());
            continue;
        }
        let text = String::from_utf8_lossy(output.stdout());
        for line in text.lines() {
            let Some(line) = list_panes_filtered_line(line, args.filter.as_deref())? else {
                continue;
            };
            lines.push(line.to_owned());
        }
    }
    if json {
        return write_list_panes_json(&CommandOutput::from_stdout(json_stdout));
    }
    write_lines_output(&lines)
}

fn list_panes_default_format(all_sessions: bool, session_scope: bool) -> &'static str {
    if all_sessions {
        DEFAULT_LIST_PANES_ALL_FORMAT
    } else if session_scope {
        DEFAULT_LIST_PANES_SESSION_FORMAT
    } else {
        DEFAULT_LIST_PANES_WINDOW_FORMAT
    }
}

fn list_panes_server_format(
    format: Option<&str>,
    filter: Option<&str>,
    default_format: &'static str,
) -> String {
    let line_format = format.unwrap_or(default_format);
    filter
        .map(|filter| format!("{filter}{LIST_PANES_FILTER_SEPARATOR}{line_format}"))
        .unwrap_or_else(|| line_format.to_owned())
}

fn list_panes_filtered_line<'a>(
    line: &'a str,
    filter: Option<&str>,
) -> Result<Option<&'a str>, ExitFailure> {
    if filter.is_none() {
        return Ok(Some(line));
    }
    let Some((filter_value, rendered_line)) = line.split_once(LIST_PANES_FILTER_SEPARATOR) else {
        return Err(ExitFailure::new(
            1,
            "list-panes filter output missing separator",
        ));
    };
    Ok(is_truthy(filter_value).then_some(rendered_line))
}

fn resolve_list_panes_target(
    connection: &mut Connection,
    target: Option<TargetSpec>,
    command_name: &str,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let Some(target) = target else {
        let session_name = resolve_session_listing_target(connection, None, command_name)?;
        let window_index = resolve_active_window_index(connection, &session_name, command_name)?;
        return Ok((session_name, Some(window_index)));
    };

    match resolve_target_spec(connection, &target, ResolveTargetType::Pane, false, false)? {
        rmux_proto::Target::Window(window_target) => Ok((
            window_target.session_name().clone(),
            Some(window_target.window_index()),
        )),
        rmux_proto::Target::Pane(pane_target) => Ok((
            pane_target.session_name().clone(),
            Some(pane_target.window_index()),
        )),
        rmux_proto::Target::Session(session_name) => {
            let window_index =
                resolve_active_window_index(connection, &session_name, command_name)?;
            Ok((session_name, Some(window_index)))
        }
    }
}

fn resolve_active_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
    command_name: &str,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some("#{window_index}:#{window_active}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let stdout = String::from_utf8_lossy(output.stdout());
    let active_line = stdout
        .lines()
        .find(|line| line.rsplit(':').next() == Some("1"))
        .ok_or_else(|| {
            ExitFailure::new(
                1,
                format!("{command_name} could not resolve the active window"),
            )
        })?;
    active_line
        .split(':')
        .next()
        .ok_or_else(|| ExitFailure::new(1, "active window output is malformed"))?
        .parse::<u32>()
        .map_err(|error| {
            ExitFailure::new(
                1,
                format!("invalid active window index from server: {error}"),
            )
        })
}

pub(super) fn run_select_pane(
    args: SelectPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let keep_zoom = args.keep_zoom;
    if args.disable_input || args.enable_input {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let target = match args.target {
            Some(target) => resolve_pane_target_spec(&mut connection, &target)?,
            None => resolve_current_pane_target(&mut connection, "select-pane")?,
        };
        let response = connection
            .select_pane_with_options(
                target,
                None,
                args.style.clone(),
                Some(args.disable_input),
                keep_zoom,
            )
            .map_err(ExitFailure::from_client)?;
        expect_command_success(response, "select-pane")?;
        return Ok(0);
    }
    if args.last {
        let mut connection = connect(socket_path)
            .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
        let target =
            resolve_window_target_or_current(&mut connection, args.target.as_ref(), "select-pane")?;
        let response = connection
            .last_pane_with_zoom(target, keep_zoom)
            .map_err(ExitFailure::from_client)?;
        expect_command_success(response, "last-pane")?;
        return Ok(0);
    }

    if let Some(direction) = args.direction() {
        let target = args.target;
        return run_command_resolved(socket_path, "select-pane", move |connection| {
            let target = match target {
                Some(target) => resolve_pane_target_spec(connection, &target)?,
                None => resolve_current_pane_target(connection, "select-pane")?,
            };
            connection
                .select_pane_adjacent_with_zoom(target, direction, keep_zoom)
                .map_err(ExitFailure::from_client)
        });
    }

    if !args.mark && !args.clear_marked {
        let title = args.title;
        let style = args.style;
        let target = args.target;
        return run_command_resolved(socket_path, "select-pane", move |connection| {
            let target = match target {
                Some(target) => resolve_pane_target_spec(connection, &target)?,
                None => resolve_current_pane_target(connection, "select-pane")?,
            };
            connection
                .select_pane_with_options(target, title.clone(), style.clone(), None, keep_zoom)
                .map_err(ExitFailure::from_client)
        });
    }

    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = match args.target {
        Some(target) => resolve_pane_target_spec(&mut connection, &target)?,
        None => resolve_current_pane_target(&mut connection, "select-pane")?,
    };
    let response = connection
        .select_pane_mark_with_title(target, args.clear_marked, args.title)
        .map_err(ExitFailure::from_client)?;
    expect_command_success(response, "select-pane")?;
    Ok(0)
}

pub(super) fn run_resize_pane(
    args: ResizePaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if !cli_target_actions_enabled() || resize_pane_uses_percent(&args) {
        return run_resize_pane_legacy(args, socket_path);
    }

    let legacy_args = args.clone();
    let target = args.target.as_ref().map(|target| target.raw().to_owned());
    let adjustment = resize_pane_adjustment(args, None);
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let response =
        connection.resize_pane_target_action(ResizePaneTargetActionRequest { target, adjustment });
    if target_action_needs_legacy_retry(&response) {
        return run_resize_pane_legacy(legacy_args, socket_path);
    }
    response
        .map_err(ExitFailure::from_client)
        .and_then(|response| {
            expect_command_success(response, "resize-pane")?;
            Ok(0)
        })
}

fn run_resize_pane_legacy(args: ResizePaneArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target =
        resolve_pane_target_or_current(&mut connection, args.target.as_ref(), "resize-pane")?;
    let window_size = resize_pane_uses_percent(&args)
        .then(|| resize_pane_window_size(&mut connection, &target))
        .transpose()?;
    let adjustment = resize_pane_adjustment(args, window_size);

    connection
        .resize_pane(target, adjustment)
        .map_err(ExitFailure::from_client)
        .and_then(|response| {
            expect_command_success(response, "resize-pane")?;
            Ok(0)
        })
}

fn resize_pane_uses_percent(args: &ResizePaneArgs) -> bool {
    args.columns
        .is_some_and(|size| matches!(size, crate::cli_args::ResizePaneSize::Percent(_)))
        || args
            .rows
            .is_some_and(|size| matches!(size, crate::cli_args::ResizePaneSize::Percent(_)))
}

fn resize_pane_adjustment(
    args: ResizePaneArgs,
    window_size: Option<(u16, u16)>,
) -> ResizePaneAdjustment {
    if args.trim_below {
        return ResizePaneAdjustment::TrimBelow;
    }
    if args.zoom {
        return ResizePaneAdjustment::Zoom;
    }
    let columns = args
        .columns
        .and_then(|size| size.resolve(window_size.map_or(0, |(width, _)| width)));
    let rows = args
        .rows
        .and_then(|size| size.resolve(window_size.map_or(0, |(_, height)| height)));
    let relative = if let Some(cells) = args.left {
        Some((ResizePaneRelativeDirection::Left, cells))
    } else if let Some(cells) = args.right {
        Some((ResizePaneRelativeDirection::Right, cells))
    } else if let Some(cells) = args.up {
        Some((ResizePaneRelativeDirection::Up, cells))
    } else {
        args.down
            .map(|cells| (ResizePaneRelativeDirection::Down, cells))
    };

    match (columns, rows, relative) {
        (Some(columns), Some(rows), Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: Some(columns),
            rows: Some(rows),
            relative: Some(relative),
            cells,
        },
        (Some(columns), None, Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: Some(columns),
            rows: None,
            relative: Some(relative),
            cells,
        },
        (None, Some(rows), Some((relative, cells))) => ResizePaneAdjustment::Composite {
            columns: None,
            rows: Some(rows),
            relative: Some(relative),
            cells,
        },
        (Some(columns), Some(rows), None) => ResizePaneAdjustment::AbsoluteSize { columns, rows },
        (Some(columns), None, None) => ResizePaneAdjustment::AbsoluteWidth { columns },
        (None, Some(rows), None) => ResizePaneAdjustment::AbsoluteHeight { rows },
        (None, None, Some((relative, cells))) => relative.to_adjustment(cells),
        (None, None, None) => ResizePaneAdjustment::NoOp,
    }
}

fn resize_pane_window_size(
    connection: &mut Connection,
    target: &rmux_proto::PaneTarget,
) -> Result<(u16, u16), ExitFailure> {
    let response = connection
        .list_panes_in_window(
            target.session_name().clone(),
            Some(target.window_index()),
            Some("#{pane_index}:#{window_width}:#{window_height}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-panes")?;
    let stdout = String::from_utf8_lossy(output.stdout());
    let pane_prefix = format!("{}:", target.pane_index());
    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(&pane_prefix))
        .ok_or_else(|| {
            ExitFailure::new(
                1,
                format!("resize-pane could not resolve dimensions for pane {target}"),
            )
        })?;
    let (width, height) = line
        .split_once(':')
        .ok_or_else(|| ExitFailure::new(1, "resize-pane dimension output is malformed"))?;
    let width = width.parse::<u16>().map_err(|error| {
        ExitFailure::new(1, format!("invalid resize-pane window width: {error}"))
    })?;
    let height = height.parse::<u16>().map_err(|error| {
        ExitFailure::new(1, format!("invalid resize-pane window height: {error}"))
    })?;
    Ok((width, height))
}
