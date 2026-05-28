use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{
    ErrorResponse, KillSessionRequest, KillWindowResponse, MoveWindowTarget, ResolveTargetType,
    Response,
};

use super::format_print::print_target_format;
use super::{
    list_session_names, resolve_current_pane_target, resolve_current_session_target,
    resolve_existing_window_target_or_current, resolve_session_listing_target,
    resolve_session_target_or_current, resolve_session_target_spec, resolve_target_spec,
    resolve_window_index_target_or_current_session, resolve_window_target_or_current,
    resolve_window_target_spec, response_name_for_target, run_command_resolved,
    unexpected_response, write_lines_output, ExitFailure,
};
use crate::cli_args::{
    AlertSessionTargetArgs, KillWindowArgs, LinkWindowArgs, ListWindowsArgs, MoveWindowArgs,
    NewWindowArgs, RenameWindowArgs, ResizeWindowArgs, RespawnWindowArgs, RotateWindowArgs,
    SessionTargetArgs, SwapWindowArgs, TargetSpec, UnlinkWindowArgs, WindowTargetArgs,
};

const DEFAULT_NEW_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

pub(super) fn run_link_window(
    args: LinkWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "link-window", move |connection| {
        let source = resolve_window_target_spec(connection, &args.source, false)?;
        let target = resolve_window_index_target_or_current_session(
            connection,
            args.target.as_ref(),
            "link-window",
        )?;
        connection
            .link_window(
                source,
                target,
                args.after,
                args.before,
                args.kill_target,
                args.detached,
            )
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_move_window(
    args: MoveWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "move-window", move |connection| {
        let request = resolve_move_window_args(connection, args)?;
        connection
            .move_window(
                request.source,
                request.target,
                request.renumber,
                request.kill_destination,
                request.detached,
            )
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_swap_window(
    args: SwapWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "swap-window", move |connection| {
        let source = resolve_window_target_spec(connection, &args.source, false)?;
        let target = resolve_existing_window_target_or_current(
            connection,
            args.target.as_ref(),
            "swap-window",
        )?;
        connection
            .swap_window(source, target, args.detached)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_rotate_window(
    args: RotateWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let direction = args.direction();
    let restore_zoom = args.restore_zoom;
    run_command_resolved(socket_path, "rotate-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "rotate-window")?;
        connection
            .rotate_window_with_zoom(target, direction, restore_zoom)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_resize_window(
    args: ResizeWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let adjust = args.adjustment.unwrap_or(1);
    let adjustment = if args.up {
        Some(rmux_proto::ResizeWindowAdjustment::Up(adjust))
    } else if args.down {
        Some(rmux_proto::ResizeWindowAdjustment::Down(adjust))
    } else if args.left {
        Some(rmux_proto::ResizeWindowAdjustment::Left(adjust))
    } else if args.right {
        Some(rmux_proto::ResizeWindowAdjustment::Right(adjust))
    } else {
        None
    };
    run_command_resolved(socket_path, "resize-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "resize-window")?;
        connection
            .resize_window(target, args.width, args.height, adjustment)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_respawn_window(
    args: RespawnWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let env = if args.environment.is_empty() {
        None
    } else {
        Some(args.environment)
    };
    run_command_resolved(socket_path, "respawn-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "respawn-window")?;
        connection
            .respawn_window_with_environment(
                target,
                args.kill,
                env,
                args.start_directory,
                (!args.command.is_empty()).then_some(args.command),
            )
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_unlink_window(
    args: UnlinkWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "unlink-window", move |connection| {
        let target = resolve_existing_window_target_or_current(
            connection,
            args.target.as_ref(),
            "unlink-window",
        )?;
        connection
            .unlink_window(target, args.kill_if_last)
            .map_err(ExitFailure::from_client)
    })
}

struct ResolvedMoveWindowArgs {
    source: Option<rmux_proto::WindowTarget>,
    target: MoveWindowTarget,
    renumber: bool,
    kill_destination: bool,
    detached: bool,
}

fn resolve_move_window_args(
    connection: &mut rmux_client::Connection,
    args: MoveWindowArgs,
) -> Result<ResolvedMoveWindowArgs, ExitFailure> {
    if !args.reindex
        && (args.source.is_none()
            || !args
                .target
                .as_ref()
                .and_then(|target| target.exact())
                .is_some_and(|target| matches!(target, rmux_proto::Target::Window(_))))
    {
        return Err(ExitFailure::new(
            1,
            "move-window requires -s source-window and -t destination-window targets",
        ));
    }

    Ok(ResolvedMoveWindowArgs {
        source: args
            .source
            .as_ref()
            .map(|target| resolve_window_target_spec(connection, target, false))
            .transpose()?,
        target: if args.reindex {
            MoveWindowTarget::Session(match args.target.as_ref() {
                Some(target) => resolve_session_target_spec(connection, target, false)?,
                None => resolve_current_session(connection)?,
            })
        } else {
            MoveWindowTarget::Window(resolve_window_target_spec(
                connection,
                args.target.as_ref().expect("validated destination target"),
                true,
            )?)
        },
        renumber: args.reindex,
        kill_destination: args.kill_target,
        detached: args.detached,
    })
}

fn resolve_current_session(
    connection: &mut rmux_client::Connection,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    match connection
        .resolve_target(None, rmux_proto::ResolveTargetType::Session, false, false)
        .map_err(ExitFailure::from_client)?
    {
        rmux_proto::Response::ResolveTarget(response) => match response.target {
            rmux_proto::Target::Session(session_name) => Ok(session_name),
            other => Err(ExitFailure::new(
                1,
                format!(
                    "resolve-target produced {} where a session target was required",
                    super::response_name_for_target(&other)
                ),
            )),
        },
        rmux_proto::Response::Error(rmux_proto::ErrorResponse { error }) => {
            Err(ExitFailure::new(1, error.to_string()))
        }
        other => Err(super::unexpected_response("resolve-target", &other)),
    }
}

pub(super) fn run_new_window(args: NewWindowArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let print_target = args.print_target;
    let print_format = args
        .format
        .clone()
        .unwrap_or_else(|| DEFAULT_NEW_WINDOW_PRINT_FORMAT.to_owned());
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let insert_at_target = args.after || args.before;
    let (target, target_window_index) = if insert_at_target {
        resolve_new_window_placement_target(
            &mut connection,
            args.target.as_ref(),
            args.after,
            "new-window",
        )?
    } else {
        resolve_new_window_target_spec(&mut connection, args.target.as_ref())?
    };
    let response = connection
        .new_window_at_with_environment(
            target,
            target_window_index,
            args.name,
            args.detached,
            (!args.environment.is_empty()).then_some(args.environment),
            args.start_directory,
            (!args.command.is_empty()).then_some(args.command),
            insert_at_target,
        )
        .map_err(ExitFailure::from_client)?;
    let target = match response {
        Response::NewWindow(response) => response.target,
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(1, error.to_string()))
        }
        other => return Err(unexpected_response("new-window", &other)),
    };

    if print_target {
        let pane = rmux_proto::PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            0,
        );
        print_target_format(
            &mut connection,
            "new-window",
            rmux_proto::Target::Pane(pane),
            &print_format,
        )?;
    }

    Ok(0)
}

fn resolve_new_window_placement_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    after: bool,
    command_name: &str,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let window = match target {
        Some(target) => resolve_window_target_spec(connection, target, false)?,
        None => {
            let pane = resolve_current_pane_target(connection, command_name)?;
            rmux_proto::WindowTarget::with_window(pane.session_name().clone(), pane.window_index())
        }
    };
    let window_index = if after {
        window.window_index().checked_add(1).ok_or_else(|| {
            ExitFailure::new(
                1,
                format!(
                    "window index space exhausted for session {}",
                    window.session_name()
                ),
            )
        })?
    } else {
        window.window_index()
    };
    Ok((window.session_name().clone(), Some(window_index)))
}

fn resolve_new_window_target_spec(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let Some(target) = target else {
        return resolve_current_session_target(connection).map(|session| (session, None));
    };

    match target.exact() {
        Some(rmux_proto::Target::Session(session_name)) => {
            return Ok((session_name.clone(), None));
        }
        Some(rmux_proto::Target::Window(window_target)) => {
            return Ok((
                window_target.session_name().clone(),
                Some(window_target.window_index()),
            ));
        }
        Some(rmux_proto::Target::Pane(_)) => {}
        None => {
            if let Some(session_name) = new_window_session_only_target(target.raw())? {
                return Ok((session_name, None));
            }
        }
    }

    match resolve_target_spec(connection, target, ResolveTargetType::Session, false, false)? {
        rmux_proto::Target::Session(session_name) => Ok((session_name, None)),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a new-window target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

fn new_window_session_only_target(
    raw_target: &str,
) -> Result<Option<rmux_proto::SessionName>, ExitFailure> {
    let Some((session_name, window_part)) = raw_target.split_once(':') else {
        return Ok(None);
    };
    if !window_part.is_empty() {
        return Ok(None);
    }
    rmux_proto::SessionName::new(session_name.to_owned())
        .map(Some)
        .map_err(|error| ExitFailure::new(1, error.to_string()))
}

pub(super) fn run_kill_window(
    args: KillWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "kill-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "kill-window")?;
        let response = connection
            .kill_window(target.clone(), args.kill_others)
            .map_err(ExitFailure::from_client)?;
        match response {
            Response::Error(ErrorResponse { error })
                if error
                    .to_string()
                    .starts_with("server error: cannot kill the only window") =>
            {
                let session_name = target.session_name().clone();
                let kill_session = connection
                    .kill_session(KillSessionRequest {
                        target: session_name,
                        kill_all_except_target: false,
                        clear_alerts: false,
                    })
                    .map_err(ExitFailure::from_client)?;
                if matches!(kill_session, Response::KillSession(_)) {
                    Ok(Response::KillWindow(KillWindowResponse { target }))
                } else {
                    Ok(kill_session)
                }
            }
            response => Ok(response),
        }
    })
}

pub(super) fn run_select_window(
    args: WindowTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "select-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "select-window")?;
        connection
            .select_window(target)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_rename_window(
    args: RenameWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "rename-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "rename-window")?;
        connection
            .rename_window(target, args.new_name)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_next_window(
    args: AlertSessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "next-window", move |connection| {
        let target =
            resolve_session_listing_target(connection, args.target.clone(), "next-window")?;
        connection
            .next_window(target, args.alerts_only)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_previous_window(
    args: AlertSessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "previous-window", move |connection| {
        let target =
            resolve_session_listing_target(connection, args.target.clone(), "previous-window")?;
        connection
            .previous_window(target, args.alerts_only)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_last_window(
    args: SessionTargetArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "last-window", move |connection| {
        let target =
            resolve_session_target_or_current(connection, args.target.as_ref(), "last-window")?;
        connection
            .last_window(target)
            .map_err(ExitFailure::from_client)
    })
}

pub(super) fn run_list_windows(
    args: ListWindowsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let targets = if args.all_sessions {
        list_session_names(&mut connection)?
    } else {
        vec![resolve_session_listing_target(
            &mut connection,
            args.target,
            "list-windows",
        )?]
    };
    let mut lines = Vec::new();
    for target in targets {
        let response = connection
            .list_windows(target, args.format.clone())
            .map_err(ExitFailure::from_client)?;
        match response {
            Response::ListWindows(response) => {
                if args.all_sessions && args.format.is_none() {
                    lines.extend(response.windows.into_iter().map(|window| {
                        format!("{}:{}", window.target.session_name(), window.rendered)
                    }));
                } else {
                    lines.extend(response.windows.into_iter().map(|window| window.rendered));
                }
            }
            Response::Error(ErrorResponse { error }) => {
                return Err(ExitFailure::new(1, error.to_string()))
            }
            other => return Err(unexpected_response("list-windows", &other)),
        }
    }
    write_lines_output(&lines)
}
