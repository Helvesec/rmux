use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{ResizePaneAdjustment, ResolveTargetType, RespawnPaneRequest};

#[path = "pane_commands/split.rs"]
mod split;
#[path = "pane_commands/transfer.rs"]
mod transfer;

use super::{
    expect_command_output, expect_command_success, list_session_names, resolve_current_pane_target,
    resolve_pane_target_or_current, resolve_pane_target_spec, resolve_session_listing_target,
    resolve_target_spec, resolve_window_target_or_current, run_command_resolved,
    shell_command_text, write_lines_output, ExitFailure,
};
use crate::cli_args::{
    ListPanesArgs, PipePaneArgs, ResizePaneArgs, RespawnPaneArgs, SelectPaneArgs, TargetSpec,
    WindowTargetArgs,
};

pub(super) use split::run_split_window;
pub(super) use transfer::{run_break_pane, run_join_pane, run_move_pane, run_swap_pane};

pub(super) fn run_last_pane(
    args: WindowTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "last-pane", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "last-pane")?;
        connection
            .last_pane(target)
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
    let pane_targets = if args.all_sessions {
        list_session_names(&mut connection)?
            .into_iter()
            .map(|session_name| (session_name, None))
            .collect::<Vec<_>>()
    } else {
        vec![resolve_list_panes_target(
            &mut connection,
            args.target,
            "list-panes",
        )?]
    };
    let mut lines = Vec::new();
    for (session_name, target_window_index) in pane_targets {
        let response = connection
            .list_panes_in_window(
                session_name.clone(),
                target_window_index,
                args.format.clone(),
            )
            .map_err(ExitFailure::from_client)?;
        let output = expect_command_output(&response, "list-panes")?;
        let text = String::from_utf8_lossy(output.stdout());
        let prefix = format!("{session_name}:");
        for line in text.lines() {
            if args.short_format && args.format.is_none() {
                lines.push(line.strip_prefix(&prefix).unwrap_or(line).to_owned());
            } else {
                lines.push(line.to_owned());
            }
        }
    }
    write_lines_output(&lines)
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

    match target.exact() {
        Some(rmux_proto::Target::Session(session_name)) => {
            let window_index = resolve_active_window_index(connection, session_name, command_name)?;
            return Ok((session_name.clone(), Some(window_index)));
        }
        Some(rmux_proto::Target::Window(window_target)) => {
            return Ok((
                window_target.session_name().clone(),
                Some(window_target.window_index()),
            ));
        }
        Some(rmux_proto::Target::Pane(pane_target)) => {
            return Ok((
                pane_target.session_name().clone(),
                Some(pane_target.window_index()),
            ));
        }
        None => {}
    }

    match resolve_target_spec(connection, &target, ResolveTargetType::Window, false, false)? {
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
    if let Some(direction) = args.direction() {
        let target = args.target;
        return run_command_resolved(socket_path, "select-pane", move |connection| {
            let target = match target {
                Some(target) => resolve_pane_target_spec(connection, &target)?,
                None => resolve_current_pane_target(connection, "select-pane")?,
            };
            connection
                .select_pane_adjacent(target, direction)
                .map_err(ExitFailure::from_client)
        });
    }

    if !args.mark && !args.clear_marked {
        let title = args.title;
        let target = args.target;
        return run_command_resolved(socket_path, "select-pane", move |connection| {
            let target = match target {
                Some(target) => resolve_pane_target_spec(connection, &target)?,
                None => resolve_current_pane_target(connection, "select-pane")?,
            };
            connection
                .select_pane_with_title(target, title.clone())
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
    let adjustment = if let Some(cells) = args.down {
        ResizePaneAdjustment::Down { cells }
    } else if let Some(cells) = args.up {
        ResizePaneAdjustment::Up { cells }
    } else if let Some(cells) = args.left {
        ResizePaneAdjustment::Left { cells }
    } else if let Some(cells) = args.right {
        ResizePaneAdjustment::Right { cells }
    } else if let (Some(columns), Some(rows)) = (args.columns, args.rows) {
        ResizePaneAdjustment::AbsoluteSize { columns, rows }
    } else if let Some(columns) = args.columns {
        ResizePaneAdjustment::AbsoluteWidth { columns }
    } else if let Some(rows) = args.rows {
        ResizePaneAdjustment::AbsoluteHeight { rows }
    } else if args.zoom {
        ResizePaneAdjustment::Zoom
    } else {
        ResizePaneAdjustment::NoOp
    };

    run_command_resolved(socket_path, "resize-pane", move |connection| {
        let target =
            resolve_pane_target_or_current(connection, args.target.as_ref(), "resize-pane")?;
        connection
            .resize_pane(target, adjustment)
            .map_err(ExitFailure::from_client)
    })
}
