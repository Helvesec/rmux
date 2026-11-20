use std::path::PathBuf;

use rmux_core::{ManagedSessionStore, OptionStore, TargetFindContext};
use rmux_proto::{
    BreakPaneRequest, KillPaneRequest, PipePaneRequest, Request, RespawnPaneRequest, RmuxError,
    SwapPaneDirection, SwapPaneRequest,
};

use super::tokens::{
    parse_compact_flag_cluster, rebuild_shell_command, CommandTokens, CompactFlag,
};
use super::values::{reject_unknown_option_before_positional, unsupported_flag};
use super::window_parse::implicit_window_index_destination;
use super::{
    implicit_pane_target, marked_pane_target_or_current, parse_pane_target, parse_window_target,
};

mod join_move;
mod select_pane;
mod split_window;

pub(super) use self::join_move::{parse_join_pane, parse_move_pane};
pub(super) use self::select_pane::parse_select_pane;
pub(super) use self::split_window::{
    parse_queued_split_window, parse_split_window, ParsedSplitWindowCommand,
};

pub(super) fn parse_pane_request(
    mut args: CommandTokens,
    command: &str,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut kill_all_except = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" if command == "kill-pane" => {
                let _ = args.optional();
                kill_all_except = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(command, args.required("-t target")?)?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(command, token, "a", "t")? else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') if command == "kill-pane" => {
                            kill_all_except = true;
                        }
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag(command, &format!("-{flag}")));
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                command,
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag(command, &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    args.no_extra(command)?;

    let target = target.unwrap_or(implicit_pane_target(sessions, find_context, command)?);
    match command {
        "kill-pane" => Ok(Request::KillPane(KillPaneRequest {
            target,
            kill_all_except,
        })),
        _ => Err(RmuxError::Server(format!(
            "unsupported pane request parser command: {command}"
        ))),
    }
}

pub(super) fn parse_swap_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut detached = false;
    let mut direction = None;
    let mut preserve_zoom = false;
    let mut source = None;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-Z" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-D" => {
                let _ = args.optional();
                if direction.is_some() {
                    return Err(RmuxError::Server(
                        "swap-pane accepts only one of -D or -U".to_owned(),
                    ));
                }
                direction = Some(SwapPaneDirection::Down);
            }
            "-U" => {
                let _ = args.optional();
                if direction.is_some() {
                    return Err(RmuxError::Server(
                        "swap-pane accepts only one of -D or -U".to_owned(),
                    ));
                }
                direction = Some(SwapPaneDirection::Up);
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_pane_target("swap-pane", args.required("-s target")?)?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("swap-pane", args.required("-t target")?)?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster("swap-pane", token, "dDUZ", "st")?
                else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare('D') => {
                            if direction.is_some() {
                                return Err(RmuxError::Server(
                                    "swap-pane accepts only one of -D or -U".to_owned(),
                                ));
                            }
                            direction = Some(SwapPaneDirection::Down);
                        }
                        CompactFlag::Bare('U') => {
                            if direction.is_some() {
                                return Err(RmuxError::Server(
                                    "swap-pane accepts only one of -D or -U".to_owned(),
                                ));
                            }
                            direction = Some(SwapPaneDirection::Up);
                        }
                        CompactFlag::Bare('Z') => preserve_zoom = true,
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            source = Some(parse_pane_target(
                                "swap-pane",
                                compact_flag.value_or_next(&mut args, "-s target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                "swap-pane",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("swap-pane", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    args.no_extra("swap-pane")?;

    let target = target.unwrap_or(implicit_pane_target(sessions, find_context, "swap-pane")?);
    if direction.is_some() && source.is_some() {
        return Err(RmuxError::Server(
            "swap-pane -D/-U does not accept -s".to_owned(),
        ));
    }
    let source = match direction {
        Some(_) => target.clone(),
        None => match source {
            Some(source) => source,
            None => marked_pane_target_or_current(sessions, find_context, "swap-pane")?,
        },
    };

    Ok(Request::SwapPane(SwapPaneRequest {
        source,
        target,
        direction,
        detached,
        preserve_zoom,
    }))
}

pub(super) fn parse_break_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut after = false;
    let mut before = false;
    let mut detached = false;
    let mut format = None;
    let mut print_target = false;
    let mut source = None;
    let mut target = None;
    let mut name = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                if before {
                    return Err(RmuxError::Server(
                        "break-pane accepts only one of -a or -b".to_owned(),
                    ));
                }
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                if after {
                    return Err(RmuxError::Server(
                        "break-pane accepts only one of -a or -b".to_owned(),
                    ));
                }
                before = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-F" => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            "-P" => {
                let _ = args.optional();
                print_target = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_pane_target(
                    "break-pane",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "break-pane",
                    args.required("-t target")?,
                )?);
            }
            "-n" => {
                let _ = args.optional();
                name = Some(args.required("-n name")?);
            }
            token => {
                let Some(cluster) =
                    parse_compact_flag_cluster("break-pane", token, "abdP", "Fstn")?
                else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => {
                            if before {
                                return Err(RmuxError::Server(
                                    "break-pane accepts only one of -a or -b".to_owned(),
                                ));
                            }
                            after = true;
                        }
                        CompactFlag::Bare('b') => {
                            if after {
                                return Err(RmuxError::Server(
                                    "break-pane accepts only one of -a or -b".to_owned(),
                                ));
                            }
                            before = true;
                        }
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare('P') => print_target = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("break-pane", &format!("-{flag}")));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'F', .. } => {
                            format = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            source = Some(parse_pane_target(
                                "break-pane",
                                compact_flag.value_or_next(&mut args, "-s target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_window_target(
                                "break-pane",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'n', .. } => {
                            name = Some(compact_flag.value_or_next(&mut args, "-n name")?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("break-pane", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    args.no_extra("break-pane")?;
    let source = source.unwrap_or(implicit_pane_target(sessions, find_context, "break-pane")?);
    // tmux 3.7b breaks into the current session, not the source pane's one.
    let target = match target {
        Some(target) => target,
        None => implicit_window_index_destination(
            sessions,
            options,
            find_context,
            after || before,
            "break-pane",
        )?,
    };

    Ok(Request::BreakPane(Box::new(BreakPaneRequest {
        source,
        target: Some(target),
        name,
        detached,
        after,
        before,
        print_target,
        format,
    })))
}

pub(super) fn parse_pipe_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut stdin = false;
    let mut stdout = false;
    let mut once = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-I" => {
                let _ = args.optional();
                stdin = true;
            }
            "-O" => {
                let _ = args.optional();
                stdout = true;
            }
            "-o" => {
                let _ = args.optional();
                once = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("pipe-pane", args.required("-t target")?)?);
            }
            token => {
                reject_unknown_option_before_positional("pipe-pane", token)?;
                break;
            }
        }
    }

    let command = {
        let remaining = args.remaining();
        (!remaining.is_empty()).then(|| rebuild_shell_command(remaining))
    };

    Ok(Request::PipePane(PipePaneRequest {
        target: target.unwrap_or(implicit_pane_target(sessions, find_context, "pipe-pane")?),
        stdin,
        stdout: if stdin || stdout { stdout } else { true },
        once,
        command,
    }))
}

pub(super) fn parse_respawn_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut kill = false;
    let mut start_directory = None;
    let mut environment = Vec::new();

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(PathBuf::from(args.required("-c start-directory")?));
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e environment")?);
            }
            "-k" => {
                let _ = args.optional();
                kill = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "respawn-pane",
                    args.required("-t target")?,
                )?);
            }
            token => {
                reject_unknown_option_before_positional("respawn-pane", token)?;
                break;
            }
        }
    }

    let command = {
        let remaining = args.remaining();
        (!remaining.is_empty()).then_some(remaining)
    };

    Ok(Request::RespawnPane(Box::new(RespawnPaneRequest {
        target: target.unwrap_or(implicit_pane_target(
            sessions,
            find_context,
            "respawn-pane",
        )?),
        kill,
        start_directory,
        environment: (!environment.is_empty()).then_some(environment),
        command,
        process_command: None,
    })))
}
