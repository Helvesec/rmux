use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{ErrorResponse, MoveWindowTarget, Response};

use super::{
    resolve_current_pane_target, resolve_existing_window_target_or_current,
    resolve_session_listing_target, resolve_session_target_or_current, resolve_session_target_spec,
    resolve_window_target_or_current, resolve_window_target_spec, run_command_resolved,
    unexpected_response, ExitFailure,
};
use crate::cli_args::{
    AlertSessionTargetArgs, KillWindowArgs, LinkWindowArgs, MoveWindowArgs, RenameWindowArgs,
    ResizeWindowArgs, RespawnWindowArgs, RotateWindowArgs, SelectWindowArgs, SessionTargetArgs,
    SwapWindowArgs, TargetSpec, UnlinkWindowArgs,
};

const LIST_WINDOWS_FILTER_SEPARATOR: char = '\x1f';

mod destination;
mod list_windows;
mod new_window;

#[cfg(test)]
use destination::link_target_is_explicit_session_only;
pub(super) use destination::resolve_link_window_target;
use destination::{
    first_available_window_index, resolve_window_destination_target,
    resolve_window_placement_anchor_target,
};
pub(super) use list_windows::run_list_windows;
#[cfg(test)]
use new_window::new_window_target_requests_window_index;
pub(super) use new_window::run_new_window;

pub(super) fn run_link_window(
    args: LinkWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "link-window", move |connection| {
        let source =
            resolve_window_target_or_current(connection, args.source.as_ref(), "link-window")?;
        let target = resolve_link_window_target(
            connection,
            args.target.as_ref(),
            args.after,
            args.before,
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
    if args.after || args.before {
        return run_move_window_relative(args, socket_path);
    }

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

fn run_move_window_relative(args: MoveWindowArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let source =
        resolve_window_target_or_current(&mut connection, args.source.as_ref(), "move-window")?;
    let target = resolve_window_placement_anchor_target(
        &mut connection,
        args.target.as_ref(),
        "move-window",
    )?;
    let response = connection
        .move_window_with_position(
            Some(source),
            MoveWindowTarget::Window(target),
            false,
            args.kill_target,
            args.detached,
            args.after,
            args.before,
        )
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::MoveWindow(_) => {}
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(1, error.to_string()))
        }
        other => return Err(unexpected_response("move-window", &other)),
    }
    Ok(0)
}

pub(super) fn run_swap_window(
    args: SwapWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "swap-window", move |connection| {
        let source = resolve_window_source_or_marked_or_current(connection, args.source.as_ref())?;
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

fn resolve_window_source_or_marked_or_current(
    connection: &mut Connection,
    source: Option<&TargetSpec>,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    if let Some(source) = source {
        return resolve_window_target_spec(connection, source, false);
    }

    let marked = crate::cli_args::parse_target_spec("{marked}")
        .map_err(|error| ExitFailure::new(1, error))?;
    match resolve_window_target_spec(connection, &marked, false) {
        Ok(target) => Ok(target),
        Err(error) if error.message().contains("{marked}") => {
            resolve_window_target_or_current(connection, None, "swap-window")
        }
        Err(error) => Err(error),
    }
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
    } else if args.expand {
        Some(rmux_proto::ResizeWindowAdjustment::LargestLinkedSession)
    } else if args.shrink {
        Some(rmux_proto::ResizeWindowAdjustment::SmallestLinkedSession)
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
    let effective_reindex = args.reindex;
    let source = if effective_reindex {
        None
    } else {
        Some(resolve_window_target_or_current(
            connection,
            args.source.as_ref(),
            "move-window",
        )?)
    };
    let target = if effective_reindex {
        match args.target.as_ref() {
            Some(target) => resolve_move_window_reindex_target(connection, target)?,
            None => MoveWindowTarget::Session(resolve_current_session(connection)?),
        }
    } else {
        debug_assert!(
            source.is_some(),
            "non-reindex move-window source is resolved"
        );
        MoveWindowTarget::Window(resolve_move_window_destination(
            connection,
            args.target.as_ref(),
        )?)
    };

    Ok(ResolvedMoveWindowArgs {
        source,
        target,
        renumber: effective_reindex,
        kill_destination: args.kill_target,
        detached: args.detached,
    })
}

fn resolve_move_window_reindex_target(
    connection: &mut rmux_client::Connection,
    target: &TargetSpec,
) -> Result<MoveWindowTarget, ExitFailure> {
    if let Some((session_part, window_part)) = target.raw().split_once(':') {
        if !window_part.is_empty() {
            return Ok(MoveWindowTarget::Window(resolve_window_destination_target(
                connection,
                target,
                "move-window",
            )?));
        }
        let session_target = crate::cli_args::parse_target_spec(session_part)
            .map_err(|error| ExitFailure::new(1, error))?;
        return resolve_session_target_spec(connection, &session_target, false)
            .map(MoveWindowTarget::Session);
    }
    resolve_session_target_spec(connection, target, false).map(MoveWindowTarget::Session)
}

fn resolve_move_window_destination(
    connection: &mut rmux_client::Connection,
    target: Option<&TargetSpec>,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    match target {
        Some(target) => resolve_window_destination_target(connection, target, "move-window"),
        None => {
            let session_name = resolve_current_session(connection)?;
            let index = first_available_window_index(connection, &session_name)?;
            Ok(rmux_proto::WindowTarget::with_window(session_name, index))
        }
    }
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

pub(super) fn run_kill_window(
    args: KillWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "kill-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "kill-window")?;
        let response = connection
            .kill_window(target, args.kill_others)
            .map_err(ExitFailure::from_client)?;
        Ok(response)
    })
}

pub(super) fn run_select_window(
    args: SelectWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "select-window", move |connection| {
        if args.next {
            let target =
                resolve_session_listing_target(connection, args.target.clone(), "select-window")?;
            return connection
                .next_window(target, false)
                .map_err(ExitFailure::from_client);
        }

        if args.previous {
            let target =
                resolve_session_listing_target(connection, args.target.clone(), "select-window")?;
            return connection
                .previous_window(target, false)
                .map_err(ExitFailure::from_client);
        }

        if args.last {
            let target =
                resolve_session_listing_target(connection, args.target.clone(), "select-window")?;
            return connection
                .last_window(target)
                .map_err(ExitFailure::from_client);
        }

        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "select-window")?;
        if args.toggle_last && window_target_is_current(connection, &target)? {
            return connection
                .last_window(target.session_name().clone())
                .map_err(ExitFailure::from_client);
        }

        connection
            .select_window(target)
            .map_err(ExitFailure::from_client)
    })
}

fn window_target_is_current(
    connection: &mut Connection,
    target: &rmux_proto::WindowTarget,
) -> Result<bool, ExitFailure> {
    let current = resolve_current_pane_target(connection, "select-window")?;
    Ok(target.session_name() == current.session_name()
        && target.window_index() == current.window_index())
}

pub(super) fn run_rename_window(
    args: RenameWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "rename-window", move |connection| {
        let target =
            resolve_window_target_or_current(connection, args.target.as_ref(), "rename-window")?;
        connection
            .rename_window(target, tmux_rename_window_name(args.new_name))
            .map_err(ExitFailure::from_client)
    })
}

fn tmux_rename_window_name(name: String) -> String {
    name.replace('\\', r"\\")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_window_signed_targets_request_window_index_resolution() {
        assert!(new_window_target_requests_window_index("+3"));
        assert!(new_window_target_requests_window_index("-1"));
        assert!(new_window_target_requests_window_index("3"));
        assert!(new_window_target_requests_window_index("alpha:+3"));
        assert!(new_window_target_requests_window_index("alpha:-1"));
        assert!(new_window_target_requests_window_index("^"));
        assert!(new_window_target_requests_window_index("!"));
    }

    #[test]
    fn new_window_session_only_targets_stay_session_scoped() {
        assert!(!new_window_target_requests_window_index("alpha"));
        assert!(!new_window_target_requests_window_index("alpha:"));
    }

    #[test]
    fn numeric_link_targets_are_window_indices() {
        let target = crate::cli_args::parse_target_spec("3").expect("target parses");
        assert!(!link_target_is_explicit_session_only(&target));
    }

    #[test]
    fn bare_session_link_targets_are_not_explicit_session_only() {
        let target = crate::cli_args::parse_target_spec("beta").expect("target parses");
        assert!(!link_target_is_explicit_session_only(&target));
    }

    #[test]
    fn colon_session_link_targets_are_explicit_session_only() {
        let target = crate::cli_args::parse_target_spec("beta:").expect("target parses");
        assert!(link_target_is_explicit_session_only(&target));
    }

    #[test]
    fn exact_session_link_targets_are_session_only() {
        let target = crate::cli_args::parse_target_spec("=beta").expect("target parses");
        assert!(link_target_is_explicit_session_only(&target));
    }

    #[test]
    fn special_link_targets_are_window_targets() {
        let target = crate::cli_args::parse_target_spec("{end}").expect("target parses");
        assert!(!link_target_is_explicit_session_only(&target));
    }

    #[test]
    fn relative_link_targets_are_window_targets() {
        let target = crate::cli_args::parse_target_spec("+1").expect("target parses");
        assert!(!link_target_is_explicit_session_only(&target));
    }
}
