use std::path::PathBuf;

use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    KillWindowRequest, LastPaneRequest, LastWindowRequest, NewWindowRequest, NextLayoutRequest,
    NextWindowRequest, PreviousLayoutRequest, PreviousWindowRequest, RenameWindowRequest, Request,
    ResizeWindowAdjustment, ResizeWindowRequest, RespawnWindowRequest, RmuxError,
    RotateWindowDirection, RotateWindowRequest, SelectWindowRequest, WindowTarget,
};

use crate::pane_terminals::session_not_found;

use super::tokens::CommandTokens;
use super::values::{missing_argument, unsupported_flag};
use super::{
    implicit_session_name, implicit_window_target, parse_new_window_target_argument,
    parse_window_target,
};

#[path = "window_parse/links.rs"]
mod links;

pub(super) use self::links::{
    parse_link_window, parse_move_window, parse_swap_window, parse_unlink_window,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectWindowMode {
    Select,
    Last,
    Next,
    Previous,
}

pub(super) fn parse_window_request(
    mut args: CommandTokens,
    command: &str,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut select_window_mode = SelectWindowMode::Select;
    let mut toggle_last = false;
    let mut preserve_zoom = false;
    let mut input_disabled = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-l" if command == "select-window" => {
                let _ = args.optional();
                set_select_window_mode(&mut select_window_mode, SelectWindowMode::Last)?;
            }
            "-n" if command == "select-window" => {
                let _ = args.optional();
                set_select_window_mode(&mut select_window_mode, SelectWindowMode::Next)?;
            }
            "-p" if command == "select-window" => {
                let _ = args.optional();
                set_select_window_mode(&mut select_window_mode, SelectWindowMode::Previous)?;
            }
            "-T" if command == "select-window" => {
                let _ = args.optional();
                toggle_last = true;
            }
            "-Z" if command == "last-pane" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-d" if command == "last-pane" => {
                let _ = args.optional();
                set_last_pane_input(&mut input_disabled, true)?;
            }
            "-e" if command == "last-pane" => {
                let _ = args.optional();
                set_last_pane_input(&mut input_disabled, false)?;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(command, args.required("-t target")?)?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag(command, flag)),
            _ => break,
        }
    }
    args.no_extra(command)?;

    let target = target.unwrap_or(implicit_window_target(sessions, find_context, command)?);
    match command {
        "select-window" => select_window_request(
            select_window_mode,
            toggle_last,
            target,
            sessions,
            find_context,
        ),
        "last-pane" => Ok(Request::LastPane(LastPaneRequest {
            target,
            preserve_zoom,
            input_disabled,
        })),
        "next-layout" => Ok(Request::NextLayout(NextLayoutRequest { target })),
        "previous-layout" => Ok(Request::PreviousLayout(PreviousLayoutRequest { target })),
        _ => Err(RmuxError::Server(format!(
            "unsupported window request parser command: {command}"
        ))),
    }
}

fn set_last_pane_input(current: &mut Option<bool>, disabled: bool) -> Result<(), RmuxError> {
    if current.is_some() {
        return Err(RmuxError::Server(
            "last-pane accepts only one of -d or -e".to_owned(),
        ));
    }
    *current = Some(disabled);
    Ok(())
}

fn set_select_window_mode(
    current: &mut SelectWindowMode,
    next: SelectWindowMode,
) -> Result<(), RmuxError> {
    if *current != SelectWindowMode::Select {
        return Err(RmuxError::Server(
            "select-window accepts only one of -l, -n, or -p".to_owned(),
        ));
    }
    *current = next;
    Ok(())
}

fn select_window_request(
    mode: SelectWindowMode,
    toggle_last: bool,
    target: WindowTarget,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let session_name = target.session_name().clone();
    match mode {
        SelectWindowMode::Last => Ok(Request::LastWindow(LastWindowRequest {
            target: session_name,
        })),
        SelectWindowMode::Next => Ok(Request::NextWindow(NextWindowRequest {
            target: session_name,
            alerts_only: false,
        })),
        SelectWindowMode::Previous => Ok(Request::PreviousWindow(PreviousWindowRequest {
            target: session_name,
            alerts_only: false,
        })),
        SelectWindowMode::Select => {
            if toggle_last
                && target == implicit_window_target(sessions, find_context, "select-window")?
            {
                return Ok(Request::LastWindow(LastWindowRequest {
                    target: session_name,
                }));
            }
            Ok(Request::SelectWindow(SelectWindowRequest { target }))
        }
    }
}

pub(super) fn parse_new_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut environment = Vec::new();
    let mut target = None;
    let mut target_window_index = None;
    let mut name = None;
    let mut detached = false;
    let mut after = false;
    let mut before = false;
    let mut target_has_signed_window_part = false;
    let mut start_directory = None;
    let mut command_only = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                command_only = true;
                break;
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(PathBuf::from(args.required("-c start-directory")?));
            }
            "-a" => {
                let _ = args.optional();
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                before = true;
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e name=value")?);
            }
            "-t" => {
                let _ = args.optional();
                let raw_target = args.required("-t target")?;
                target_has_signed_window_part =
                    signed_window_target_session_part(&raw_target).is_some();
                let (session_name, window_index) =
                    parse_new_window_target_argument(raw_target, sessions, find_context)?;
                target = Some(session_name);
                target_window_index = window_index;
            }
            "-n" => {
                let _ = args.optional();
                name = Some(args.required("-n name")?);
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            _ => break,
        }
    }

    if !command_only && args.peek().is_some_and(|token| token.starts_with('-')) {
        args.no_extra("new-window")?;
    }

    let command = {
        let remaining = args.remaining();
        (!remaining.is_empty()).then_some(remaining)
    };

    let insert_at_target = after || before;
    if insert_at_target {
        if target_window_index.is_none() || target_has_signed_window_part {
            let window_target = if let Some(session_name) = target.as_ref() {
                let window_index = sessions
                    .session(session_name)
                    .ok_or_else(|| session_not_found(session_name))?
                    .active_window_index();
                WindowTarget::with_window(session_name.clone(), window_index)
            } else {
                implicit_window_target(sessions, find_context, "new-window")?
            };
            target = Some(window_target.session_name().clone());
            target_window_index = Some(window_target.window_index());
        }
        if after {
            target_window_index = Some(
                target_window_index
                    .expect("placement target index must exist")
                    .checked_add(1)
                    .ok_or_else(|| {
                        RmuxError::Server("window index space exhausted for new-window".to_owned())
                    })?,
            );
        }
    }

    Ok(Request::NewWindow(Box::new(NewWindowRequest {
        target: target.unwrap_or(implicit_session_name(sessions, find_context, "new-window")?),
        name,
        detached,
        start_directory,
        environment: (!environment.is_empty()).then_some(environment),
        command,
        process_command: None,
        target_window_index,
        insert_at_target,
    })))
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

fn signed_window_index_target(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn parse_rename_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "rename-window",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }

    let name = args.required("rename-window new-name")?;
    args.no_extra("rename-window")?;

    Ok(Request::RenameWindow(RenameWindowRequest {
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "rename-window",
        )?),
        name: tmux_rename_window_name(name),
    }))
}

fn tmux_rename_window_name(name: String) -> String {
    name.replace('\\', r"\\")
}

pub(super) fn parse_kill_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut kill_all_others = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-a" => kill_all_others = true,
            "-t" => {
                target = Some(parse_window_target(
                    "kill-window",
                    args.required("-t target")?,
                )?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("kill-window", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for kill-window"
                )));
            }
        }
    }

    Ok(Request::KillWindow(KillWindowRequest {
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "kill-window",
        )?),
        kill_all_others,
    }))
}

pub(super) fn parse_rotate_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut direction = RotateWindowDirection::Up;
    let mut direction_set = false;
    let mut target = None;
    let mut restore_zoom = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-D" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "rotate-window accepts only one of -D or -U".to_owned(),
                    ));
                }
                direction = RotateWindowDirection::Down;
                direction_set = true;
            }
            "-U" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "rotate-window accepts only one of -D or -U".to_owned(),
                    ));
                }
                direction = RotateWindowDirection::Up;
                direction_set = true;
            }
            "-Z" => {
                let _ = args.optional();
                restore_zoom = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "rotate-window",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }
    args.no_extra("rotate-window")?;

    Ok(Request::RotateWindow(RotateWindowRequest {
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "rotate-window",
        )?),
        direction,
        restore_zoom,
    }))
}

pub(super) fn parse_resize_window(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut width = None;
    let mut height = None;
    let mut adjustment = None;
    let mut adjust_amount: Option<u16> = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "resize-window",
                    args.required("-t target")?,
                )?);
            }
            "-x" => {
                let _ = args.optional();
                let value = args.required("-x width")?;
                width = Some(value.parse::<u16>().map_err(|_| {
                    RmuxError::Server(format!("resize-window: invalid width: {value}"))
                })?);
            }
            "-y" => {
                let _ = args.optional();
                let value = args.required("-y height")?;
                height = Some(value.parse::<u16>().map_err(|_| {
                    RmuxError::Server(format!("resize-window: invalid height: {value}"))
                })?);
            }
            "-D" => {
                let _ = args.optional();
                adjustment = Some("D");
            }
            "-U" => {
                let _ = args.optional();
                adjustment = Some("U");
            }
            "-L" => {
                let _ = args.optional();
                adjustment = Some("L");
            }
            "-R" => {
                let _ = args.optional();
                adjustment = Some("R");
            }
            "-A" => {
                let _ = args.optional();
                adjustment = Some("A");
            }
            "-a" => {
                let _ = args.optional();
                adjustment = Some("a");
            }
            _ => {
                if let Some(value) = args.optional() {
                    adjust_amount = Some(value.parse::<u16>().map_err(|_| {
                        RmuxError::Server(format!("resize-window: invalid adjustment: {value}"))
                    })?);
                }
                break;
            }
        }
    }
    args.no_extra("resize-window")?;

    let adjustment = adjustment.map(|dir| {
        let amount = adjust_amount.unwrap_or(1);
        match dir {
            "D" => ResizeWindowAdjustment::Down(amount),
            "U" => ResizeWindowAdjustment::Up(amount),
            "L" => ResizeWindowAdjustment::Left(amount),
            "R" => ResizeWindowAdjustment::Right(amount),
            "A" => ResizeWindowAdjustment::LargestLinkedSession,
            "a" => ResizeWindowAdjustment::SmallestLinkedSession,
            _ => unreachable!(),
        }
    });

    Ok(Request::ResizeWindow(ResizeWindowRequest {
        target: target.ok_or_else(|| missing_argument("resize-window", "-t target"))?,
        width,
        height,
        adjustment,
    }))
}

pub(super) fn parse_respawn_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut kill = false;
    let mut start_directory = None;
    let mut environment = Vec::new();
    let mut command_only = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                command_only = true;
                break;
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(PathBuf::from(args.required("-c start-directory")?));
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "respawn-window",
                    args.required("-t target")?,
                )?);
            }
            "-k" => {
                let _ = args.optional();
                kill = true;
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e environment")?.to_owned());
            }
            _ => break,
        }
    }

    if !command_only && args.peek().is_some_and(|token| token.starts_with('-')) {
        args.no_extra("respawn-window")?;
    }

    let command = {
        let remaining = args.remaining();
        (!remaining.is_empty()).then_some(remaining)
    };

    let environment = if environment.is_empty() {
        None
    } else {
        Some(environment)
    };

    Ok(Request::RespawnWindow(Box::new(RespawnWindowRequest {
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "respawn-window",
        )?),
        kill,
        start_directory,
        environment,
        command,
    })))
}
