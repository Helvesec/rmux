use std::path::PathBuf;

use rmux_core::{ManagedSessionStore, OptionStore, TargetFindContext};
use rmux_proto::{
    KillWindowRequest, LastPaneRequest, LastWindowRequest, NextLayoutRequest, NextWindowRequest,
    PreviousLayoutRequest, PreviousWindowRequest, RenameWindowRequest, Request,
    ResizeWindowAdjustment, ResizeWindowRequest, RespawnWindowRequest, RmuxError,
    RotateWindowDirection, RotateWindowRequest, SelectWindowRequest, WindowTarget,
};

use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{reject_unknown_option_before_positional, unsupported_flag};
use super::{implicit_session_name, implicit_window_target, parse_window_target};

mod links;
mod new_window;

use self::links::first_available_window_target;
pub(super) use self::links::{
    parse_link_window, parse_move_window, parse_swap_window, parse_unlink_window,
};
pub(super) use self::new_window::parse_new_window;

/// Resolves an omitted `-t` of a window-index destination (`link-window`,
/// `break-pane`) the way tmux 3.7b does: the current session, anchored on its
/// current window for `-a`/`-b`, otherwise its first free index from
/// `base-index`. The current session is never inferred from the source.
pub(super) fn implicit_window_index_destination(
    sessions: &ManagedSessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
    placement: bool,
    command_name: &str,
) -> Result<WindowTarget, RmuxError> {
    if placement {
        return implicit_placement_anchor(sessions, find_context, command_name);
    }
    let session_name = implicit_session_name(sessions, find_context, command_name)?;
    first_available_window_target(sessions, options, &session_name, command_name)
}

/// The window an omitted `-t` anchors `-a`/`-b` on. A queue state that pins
/// its current target (hook, key binding, `-t`) anchors on that window; a
/// current derived from the requesting client anchors on its session's
/// active window, as tmux 3.7b `cmd_find_from_client` uses `s->curw` even
/// when the caller's pane sits in another window.
fn implicit_placement_anchor(
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<WindowTarget, RmuxError> {
    let current = implicit_window_target(sessions, find_context, command_name)?;
    if !find_context.current_follows_client() {
        return Ok(current);
    }
    let Some(session) = sessions.session(current.session_name()) else {
        return Ok(current);
    };
    Ok(WindowTarget::with_window(
        current.session_name().clone(),
        session.active_window_index(),
    ))
}

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
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut select_window_mode = SelectWindowMode::Select;
    let mut toggle_last = false;
    let mut preserve_zoom = false;
    let mut input_disabled = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
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
                set_last_pane_input(&mut input_disabled, true);
            }
            "-e" if command == "last-pane" => {
                let _ = args.optional();
                set_last_pane_input(&mut input_disabled, false);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(command, args.required("-t target")?)?);
            }
            _ => {
                let (bare_flags, value_flags) = if command == "last-pane" {
                    ("deZ", "t")
                } else {
                    ("", "")
                };
                let Some(cluster) =
                    parse_compact_flag_cluster(command, &token, bare_flags, value_flags)?
                else {
                    if token.starts_with('-') {
                        return Err(unsupported_flag(command, &token));
                    }
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('Z') if command == "last-pane" => {
                            preserve_zoom = true;
                        }
                        CompactFlag::Bare('d') if command == "last-pane" => {
                            set_last_pane_input(&mut input_disabled, true);
                        }
                        CompactFlag::Bare('e') if command == "last-pane" => {
                            set_last_pane_input(&mut input_disabled, false);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. }
                            if command == "last-pane" =>
                        {
                            target = Some(parse_window_target(
                                command,
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        _ => unreachable!("compact window request flags are prevalidated"),
                    }
                }
            }
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

fn set_last_pane_input(current: &mut Option<bool>, disabled: bool) {
    // tmux 3.7b accepts both flags and gives -e precedence regardless of order.
    if !disabled || current.is_none() {
        *current = Some(disabled);
    }
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
    sessions: &ManagedSessionStore,
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

pub(super) fn parse_rename_window(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
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
            token => {
                reject_unknown_option_before_positional("rename-window", token)?;
                break;
            }
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
    sessions: &ManagedSessionStore,
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
            flag if flag.starts_with('-') => {
                let Some(cluster) = parse_compact_flag_cluster("kill-window", flag, "a", "t")?
                else {
                    return Err(unsupported_flag("kill-window", flag));
                };
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => kill_all_others = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("kill-window", &format!("-{flag}")));
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_window_target(
                                "kill-window",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("kill-window", &format!("-{flag}")));
                        }
                    }
                }
            }
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
    sessions: &ManagedSessionStore,
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

pub(super) fn parse_resize_window(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
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
                width = Some(rmux_core::parse_window_dimension(&value, "width")?);
            }
            "-y" => {
                let _ = args.optional();
                let value = args.required("-y height")?;
                height = Some(rmux_core::parse_window_dimension(&value, "height")?);
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
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "resize-window",
        )?),
        width,
        height,
        adjustment,
    }))
}

pub(super) fn parse_respawn_window(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
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
