use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_core::formats::{is_truthy, DEFAULT_LIST_WINDOWS_ALL_FORMAT, DEFAULT_LIST_WINDOWS_FORMAT};
use rmux_proto::{
    CommandOutput, ErrorResponse, KillSessionRequest, KillWindowResponse, ListWindowsResponse,
    MoveWindowTarget, OptionScopeSelector, ResolveTargetType, Response, WindowListEntry,
};

use super::format_print::print_target_format;
use super::json_output::write_list_windows_json;
use super::{
    expect_command_output, expect_command_success, list_session_names, resolve_current_pane_target,
    resolve_current_session_target, resolve_existing_window_target_or_current,
    resolve_session_listing_target, resolve_session_target_or_current, resolve_session_target_spec,
    resolve_target_spec, resolve_window_index_target_or_current_session,
    resolve_window_target_or_current, resolve_window_target_spec, response_name_for_target,
    run_command_resolved, unexpected_response, write_lines_output, ExitFailure,
};
use crate::cli_args::{
    AlertSessionTargetArgs, KillWindowArgs, LinkWindowArgs, ListWindowsArgs, MoveWindowArgs,
    NewWindowArgs, RenameWindowArgs, ResizeWindowArgs, RespawnWindowArgs, RotateWindowArgs,
    SelectWindowArgs, SessionTargetArgs, SwapWindowArgs, TargetSpec, UnlinkWindowArgs,
};

const DEFAULT_NEW_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";
const LIST_WINDOWS_FILTER_SEPARATOR: char = '\x1f';
const LIST_WINDOWS_SORT_METADATA_FORMAT: &str = "#{window_activity}\x1f#{window_created}";

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

fn resolve_link_window_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    after: bool,
    before: bool,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    let Some(target) = target else {
        if after || before {
            return resolve_window_placement_anchor_target(connection, None, command_name);
        }
        let session_name = resolve_session_target_or_current(connection, None, command_name)?;
        let index = first_available_window_index(connection, &session_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    };

    if after || before {
        return resolve_window_placement_anchor_target(connection, Some(target), command_name);
    }
    resolve_window_destination_target(connection, target, command_name)
}

fn link_target_is_explicit_session_only(target: &TargetSpec) -> bool {
    let raw = target.raw();
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if matches!(parse_bare_relative_window_offset(raw), Ok(Some(_))) {
        return false;
    }
    if is_special_window_token(raw) {
        return false;
    }
    if raw
        .strip_suffix(':')
        .is_some_and(|session| !session.is_empty())
    {
        return true;
    }
    raw.starts_with('=') && matches!(target.exact(), Some(rmux_proto::Target::Session(_)))
}

fn is_special_window_token(raw: &str) -> bool {
    matches!(
        raw,
        "^" | "!" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
    )
}

fn resolve_window_destination_target(
    connection: &mut Connection,
    target: &TargetSpec,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    if let Some(target) =
        resolve_bare_relative_window_target(connection, target.raw(), command_name)?
    {
        return Ok(target);
    }
    if let Some(index) = parse_bare_window_index(target.raw())? {
        let session_name = resolve_session_target_or_current(connection, None, command_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    }
    if let Some(rmux_proto::Target::Window(target)) = target.exact() {
        return Ok(target.clone());
    }
    if link_target_is_explicit_session_only(target) {
        let session_name = resolve_session_only_destination(connection, target)?;
        let index = first_available_window_index(connection, &session_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    }
    if let Some(target) =
        resolve_exact_current_window_name_destination(connection, target.raw(), command_name)?
    {
        return Ok(target);
    }
    if let Some(target) = resolve_bare_session_window_destination(connection, target)? {
        return Ok(target);
    }

    resolve_window_index_target_or_current_session(connection, Some(target), command_name)
}

fn resolve_exact_current_window_name_destination(
    connection: &mut Connection,
    raw_target: &str,
    command_name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    if !link_target_is_bare_session_candidate(raw_target) {
        return Ok(None);
    }
    let session_name = resolve_session_target_or_current(connection, None, command_name)?;
    find_window_by_name(connection, &session_name, raw_target)
}

fn resolve_bare_session_window_destination(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    if !link_target_is_bare_session_candidate(target.raw()) {
        return Ok(None);
    }
    let session_name = match resolve_session_target_spec(connection, target, false) {
        Ok(session_name) => session_name,
        Err(_) => return Ok(None),
    };
    let index = first_available_window_index(connection, &session_name)?;
    Ok(Some(rmux_proto::WindowTarget::with_window(
        session_name,
        index,
    )))
}

fn link_target_is_bare_session_candidate(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.contains([':', '.'])
        && !raw.starts_with(['@', '%', '+', '-', '='])
        && raw.parse::<u32>().is_err()
        && !matches!(
            raw,
            "!" | "^" | "$" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
        )
}

fn resolve_bare_relative_window_target(
    connection: &mut Connection,
    raw_target: &str,
    command_name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let Some(offset) = parse_bare_relative_window_offset(raw_target)? else {
        return Ok(None);
    };
    let current = resolve_window_target_or_current(connection, None, command_name)?;
    let index = apply_window_index_offset(current.window_index(), offset)?;
    Ok(Some(rmux_proto::WindowTarget::with_window(
        current.session_name().clone(),
        index,
    )))
}

fn resolve_window_placement_anchor_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    let Some(target) = target else {
        let pane = resolve_current_pane_target(connection, command_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(
            pane.session_name().clone(),
            pane.window_index(),
        ));
    };

    if let Some(session_target) = signed_window_target_session_part(target.raw()) {
        if let Some(session_target) = session_target {
            let session_target = crate::cli_args::parse_target_spec(session_target)
                .map_err(|error| ExitFailure::new(1, error))?;
            let session_name = resolve_session_target_spec(connection, &session_target, false)?;
            let window_index =
                resolve_active_window_index(connection, &session_name, command_name)?;
            return Ok(rmux_proto::WindowTarget::with_window(
                session_name,
                window_index,
            ));
        }
        return resolve_window_target_or_current(connection, None, command_name);
    }

    resolve_window_target_spec(connection, target, false)
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
    for line in stdout.lines() {
        let Some((index, active)) = line.split_once(':') else {
            continue;
        };
        if active == "1" {
            return index.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("{command_name}: invalid active window: {error}"))
            });
        }
    }
    Err(ExitFailure::new(
        1,
        format!("{command_name}: no active window in session {session_name}"),
    ))
}

fn signed_window_target_session_part(raw_target: &str) -> Option<Option<&str>> {
    if signed_window_index_target(raw_target) {
        return Some(None);
    }
    let (session, window) = raw_target.split_once(':')?;
    if session.is_empty() || !signed_window_index_target(window) {
        return None;
    }
    Some(Some(session))
}

fn parse_bare_relative_window_offset(value: &str) -> Result<Option<i64>, ExitFailure> {
    let Some(sign) = value.as_bytes().first().copied() else {
        return Ok(None);
    };
    if !matches!(sign, b'+' | b'-') {
        return Ok(None);
    }
    let rest = &value[1..];
    let magnitude = if rest.is_empty() {
        1
    } else if rest.bytes().all(|byte| byte.is_ascii_digit()) {
        rest.parse::<i64>()
            .map_err(|error| ExitFailure::new(1, format!("invalid window index: {error}")))?
    } else {
        return Ok(None);
    };
    Ok(Some(if sign == b'-' { -magnitude } else { magnitude }))
}

fn apply_window_index_offset(index: u32, offset: i64) -> Result<u32, ExitFailure> {
    let next = i64::from(index) + offset;
    if next < 0 || next > i64::from(u32::MAX) {
        return Err(ExitFailure::new(1, format!("can't find window: {next}")));
    }
    Ok(next as u32)
}

fn parse_bare_window_index(value: &str) -> Result<Option<u32>, ExitFailure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|error| ExitFailure::new(1, format!("invalid window index: {error}")))
}

fn resolve_session_only_destination(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    if let Some(session) = target.raw().strip_suffix(':') {
        let session_target = crate::cli_args::parse_target_spec(session)
            .map_err(|error| ExitFailure::new(1, error))?;
        return resolve_session_target_spec(connection, &session_target, false);
    }
    resolve_session_target_spec(connection, target, false)
}

fn first_available_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_windows(session_name.clone(), Some("#{window_index}".to_owned()))
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let used = String::from_utf8_lossy(output.stdout())
        .lines()
        .map(|line| {
            line.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("invalid list-windows index: {error}"))
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let base_index = session_base_index(connection, session_name)?;
    let mut index = base_index;
    loop {
        if !used.contains(&index) {
            return Ok(index);
        }
        let Some(next_index) = index.checked_add(1) else {
            return Err(ExitFailure::new(1, "window index space exhausted"));
        };
        index = next_index;
    }
}

fn session_base_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, ExitFailure> {
    let response = connection
        .show_options(
            OptionScopeSelector::Session(session_name.clone()),
            Some("base-index".to_owned()),
            true,
            true,
            false,
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "show-options")?;
    let value = String::from_utf8_lossy(output.stdout());
    value
        .trim()
        .parse::<u32>()
        .map_err(|error| ExitFailure::new(1, format!("invalid base-index value: {error}")))
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

pub(super) fn run_new_window(args: NewWindowArgs, socket_path: &Path) -> Result<i32, ExitFailure> {
    let print_target = args.print_target;
    let print_format = args
        .format
        .clone()
        .unwrap_or_else(|| DEFAULT_NEW_WINDOW_PRINT_FORMAT.to_owned());
    let name = args.name.clone();
    let kill_existing = args.kill_existing;
    let select_existing = args.select_existing;
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
    if select_existing {
        if let Some(existing) = name
            .as_deref()
            .and_then(|name| find_window_by_name(&mut connection, &target, name).transpose())
            .transpose()?
        {
            if !args.detached {
                connection
                    .select_window(existing.clone())
                    .map_err(ExitFailure::from_client)?;
            }
            if print_target {
                let pane = rmux_proto::PaneTarget::with_window(
                    existing.session_name().clone(),
                    existing.window_index(),
                    0,
                );
                print_target_format(
                    &mut connection,
                    "new-window",
                    rmux_proto::Target::Pane(pane),
                    &print_format,
                )?;
            }
            return Ok(0);
        }
    }
    let mut create_window_index = target_window_index;
    let replace_after_create = if kill_existing {
        match target_window_index {
            Some(window_index) => {
                match kill_existing_window_at(&mut connection, &target, window_index) {
                    Ok(()) => None,
                    Err(error) if only_window_kill_failure(&error) => {
                        create_window_index = None;
                        Some(window_index)
                    }
                    Err(error) => return Err(error),
                }
            }
            None => None,
        }
    } else {
        None
    };
    let response = connection
        .new_window_at_with_environment(
            target.clone(),
            create_window_index,
            name,
            args.detached,
            (!args.environment.is_empty()).then_some(args.environment),
            args.start_directory,
            (!args.command.is_empty()).then_some(args.command),
            insert_at_target,
        )
        .map_err(ExitFailure::from_client)?;
    let target = match response {
        Response::NewWindow(response) => response.target,
        response @ Response::Error(_) => {
            expect_command_success(response, "new-window")?;
            unreachable!("new-window error response should return from expect_command_success")
        }
        other => return Err(unexpected_response("new-window", &other)),
    };
    let target = if let Some(window_index) = replace_after_create {
        move_created_window_to_replacement_index(
            &mut connection,
            target,
            window_index,
            args.detached,
        )?
    } else {
        target
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

fn find_window_by_name(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
    name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some(format!(
                "#{{window_index}}{LIST_WINDOWS_FILTER_SEPARATOR}#{{window_name}}"
            )),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    for line in String::from_utf8_lossy(output.stdout()).lines() {
        let Some((index, window_name)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
            continue;
        };
        if window_name == name {
            let window_index = index
                .parse::<u32>()
                .map_err(|_| ExitFailure::new(1, format!("invalid window index: {index}")))?;
            return Ok(Some(rmux_proto::WindowTarget::with_window(
                session_name.clone(),
                window_index,
            )));
        }
    }
    Ok(None)
}

fn kill_existing_window_at(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) -> Result<(), ExitFailure> {
    let target = rmux_proto::WindowTarget::with_window(session_name.clone(), window_index);
    let response = connection
        .kill_window(target, false)
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::KillWindow(_) => Ok(()),
        Response::Error(ErrorResponse { error }) if missing_window_error(&error) => Ok(()),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("kill-window", &other)),
    }
}

fn only_window_kill_failure(error: &ExitFailure) -> bool {
    error
        .message()
        .contains("cannot kill the only window in session ")
}

fn move_created_window_to_replacement_index(
    connection: &mut Connection,
    created: rmux_proto::WindowTarget,
    window_index: u32,
    detached: bool,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    let target =
        rmux_proto::WindowTarget::with_window(created.session_name().clone(), window_index);
    let response = connection
        .move_window(
            Some(created),
            MoveWindowTarget::Window(target.clone()),
            false,
            true,
            detached,
        )
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::MoveWindow(response) => Ok(response.target.unwrap_or(target)),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("move-window", &other)),
    }
}

fn missing_window_error(error: &rmux_proto::RmuxError) -> bool {
    match error {
        rmux_proto::RmuxError::InvalidTarget { reason, .. } => {
            reason.contains("window index does not exist")
                || reason.starts_with("can't find window:")
        }
        rmux_proto::RmuxError::Server(message) => message.starts_with("can't find window:"),
        _ => false,
    }
}

fn resolve_new_window_placement_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    after: bool,
    command_name: &str,
) -> Result<(rmux_proto::SessionName, Option<u32>), ExitFailure> {
    let window = resolve_window_placement_anchor_target(connection, target, command_name)?;
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

    if let Some(window) =
        resolve_bare_relative_window_target(connection, target.raw(), "new-window")?
    {
        return Ok((window.session_name().clone(), Some(window.window_index())));
    }

    if new_window_target_requests_window_index(target.raw()) {
        return match resolve_target_spec(
            connection,
            target,
            ResolveTargetType::Window,
            true,
            false,
        )? {
            rmux_proto::Target::Window(window) => {
                Ok((window.session_name().clone(), Some(window.window_index())))
            }
            other => Err(ExitFailure::new(
                1,
                format!(
                    "resolve-target produced {} where a new-window target was required",
                    response_name_for_target(&other)
                ),
            )),
        };
    }

    if let Some(window_target) = resolve_new_window_bare_window_target(connection, target)? {
        return Ok((
            window_target.session_name().clone(),
            Some(window_target.window_index()),
        ));
    }

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
            if let Some(session_name) = resolve_new_window_session_only_target(connection, target)?
            {
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

fn new_window_target_requests_window_index(raw_target: &str) -> bool {
    if is_special_window_token(raw_target) {
        return true;
    }
    if !raw_target.is_empty() && raw_target.bytes().all(|byte| byte.is_ascii_digit()) {
        return true;
    }
    if signed_window_index_target(raw_target) {
        return true;
    }
    raw_target.split_once(':').is_some_and(|(_, window_part)| {
        (!window_part.is_empty() && !window_part.contains('.'))
            || signed_window_index_target(window_part)
    })
}

fn signed_window_index_target(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.chars().all(|character| character.is_ascii_digit())
}

fn resolve_new_window_bare_window_target(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let raw = target.raw();
    if !new_window_target_is_bare_lookup(raw)
        || !matches!(target.exact(), None | Some(rmux_proto::Target::Session(_)))
    {
        return Ok(None);
    }

    match resolve_target_spec(connection, target, ResolveTargetType::Window, false, false) {
        Ok(rmux_proto::Target::Window(window)) => {
            if window_name_matches_target(connection, &window, raw)? {
                Ok(Some(window))
            } else {
                Ok(None)
            }
        }
        Ok(_) => Ok(None),
        Err(window_error) => match resolve_session_target_spec(connection, target, false) {
            Ok(_) => Ok(None),
            Err(_) => Err(window_error),
        },
    }
}

fn new_window_target_is_bare_lookup(raw_target: &str) -> bool {
    !raw_target.is_empty()
        && !raw_target.starts_with(['@', '$', '%', '+', '-', '='])
        && !raw_target.contains([':', '.'])
}

fn window_name_matches_target(
    connection: &mut Connection,
    window: &rmux_proto::WindowTarget,
    target: &str,
) -> Result<bool, ExitFailure> {
    let response = connection
        .list_windows(
            window.session_name().clone(),
            Some(format!(
                "#{{window_index}}{LIST_WINDOWS_FILTER_SEPARATOR}#{{window_name}}"
            )),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    for line in String::from_utf8_lossy(output.stdout()).lines() {
        let Some((index, window_name)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
            continue;
        };
        if index != window.window_index().to_string() {
            continue;
        }
        return Ok(window_name == target
            || window_name.starts_with(target)
            || rmux_core::fnmatch(target, window_name));
    }
    Ok(false)
}

fn resolve_new_window_session_only_target(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::SessionName>, ExitFailure> {
    let raw_target = target.raw();
    let Some((session_name, window_part)) = raw_target.split_once(':') else {
        return Ok(None);
    };
    if !window_part.is_empty() {
        return Ok(None);
    }
    let session_target = crate::cli_args::parse_target_spec(session_name)
        .map_err(|error| ExitFailure::new(1, error))?;
    resolve_session_target_spec(connection, &session_target, false).map(Some)
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
                        kill_group: false,
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

pub(super) fn run_list_windows(
    args: ListWindowsArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let json = args.json;
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
    let sort_order = CliWindowListSortOrder::parse(args.sort_order.as_deref())?;
    let include_sort_metadata = args.all_sessions
        && matches!(
            sort_order,
            CliWindowListSortOrder::Activity | CliWindowListSortOrder::Creation
        );
    let format = list_windows_server_format(
        args.format.as_deref(),
        args.filter.as_deref(),
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
                    .filter_map(|mut window| {
                        let rendered = match list_windows_filtered_line(
                            &window.rendered,
                            args.filter.as_deref(),
                        ) {
                            Ok(rendered) => rendered?,
                            Err(error) => return Some(Err(error)),
                        };
                        let (activity_at, created_at, rendered) =
                            match list_windows_sort_metadata(rendered, include_sort_metadata) {
                                Ok(parsed) => parsed,
                                Err(error) => return Some(Err(error)),
                            };
                        window.rendered = rendered.to_owned();
                        Some(Ok(CliWindowListEntry {
                            window,
                            activity_at,
                            created_at,
                        }))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
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
        return write_list_windows_json(&ListWindowsResponse {
            windows: windows.into_iter().map(|window| window.window).collect(),
            output: CommandOutput::from_stdout(Vec::new()),
        });
    }
    write_lines_output(&lines)
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
            CliWindowListSortOrder::Size => (left.window.size.cols, left.window.size.rows)
                .cmp(&(right.window.size.cols, right.window.size.rows)),
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

fn stable_window_entry_name_cmp(left: &CliWindowListEntry, right: &CliWindowListEntry) -> Ordering {
    left.window.name.cmp(&right.window.name)
}

fn list_windows_server_format(
    format: Option<&str>,
    filter: Option<&str>,
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
    filter
        .map(|filter| {
            let line_format = line_format.as_deref().unwrap_or(default_format);
            format!("{filter}{LIST_WINDOWS_FILTER_SEPARATOR}{line_format}")
        })
        .or(line_format)
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

fn list_windows_filtered_line<'a>(
    line: &'a str,
    filter: Option<&str>,
) -> Result<Option<&'a str>, ExitFailure> {
    if filter.is_none() {
        return Ok(Some(line));
    }
    let Some((filter_value, rendered_line)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
        return Err(ExitFailure::new(
            1,
            "list-windows filter output missing separator",
        ));
    };
    Ok(is_truthy(filter_value).then_some(rendered_line))
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
