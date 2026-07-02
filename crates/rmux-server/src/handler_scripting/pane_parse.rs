use std::path::PathBuf;

use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    BreakPaneRequest, KillPaneRequest, LastPaneRequest, PipePaneRequest, ProcessCommand, Request,
    RespawnPaneRequest, RmuxError, SelectPaneAdjacentRequest, SelectPaneDirection,
    SelectPaneMarkRequest, SelectPaneRequest, SplitDirection, SplitWindowExtRequest,
    SplitWindowRequest, SwapPaneDirection, SwapPaneRequest, WindowTarget,
};

use super::tokens::{
    parse_compact_flag_cluster, rebuild_shell_command, CommandTokens, CompactFlag,
};
use super::values::unsupported_flag;
use super::{
    implicit_pane_target, implicit_split_target, marked_pane_target_or_current, parse_pane_target,
    parse_split_window_target, parse_window_target,
};

const DEFAULT_SPLIT_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

#[path = "pane_parse/join_move.rs"]
mod join_move;

pub(super) use self::join_move::{parse_join_pane, parse_move_pane};

#[derive(Debug, Clone)]
pub(super) struct ParsedSplitWindowCommand {
    pub(super) request: Request,
    pub(super) print_target: bool,
    pub(super) format: String,
}

pub(super) fn parse_pane_request(
    mut args: CommandTokens,
    command: &str,
    sessions: &SessionStore,
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
                let Some(cluster) = parse_compact_flag_cluster(token, "a", "t") else {
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

pub(super) fn parse_select_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut mark = false;
    let mut clear_marked = false;
    let mut title = None;
    let mut style = None;
    let mut direction = None;
    let mut last = false;
    let mut input_disabled = None;
    let mut preserve_zoom = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "select-pane",
                    args.required("-t target")?,
                )?);
            }
            "-m" => {
                let _ = args.optional();
                mark = true;
            }
            "-M" => {
                let _ = args.optional();
                clear_marked = true;
            }
            "-l" => {
                let _ = args.optional();
                last = true;
            }
            "-Z" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-d" => {
                let _ = args.optional();
                input_disabled = Some(true);
            }
            "-e" => {
                let _ = args.optional();
                input_disabled = Some(false);
            }
            "-T" => {
                let _ = args.optional();
                title = Some(args.required("-T title")?.to_owned());
            }
            "-P" => {
                let _ = args.optional();
                style = Some(args.required("-P style")?.to_owned());
            }
            "-U" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Up);
            }
            "-D" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Down);
            }
            "-L" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Left);
            }
            "-R" => {
                let _ = args.optional();
                direction = Some(SelectPaneDirection::Right);
            }
            _ => break,
        }
    }
    args.no_extra("select-pane")?;

    if mark && clear_marked {
        return Err(RmuxError::Server(
            "select-pane flags -m and -M cannot be used together".to_owned(),
        ));
    }
    if direction.is_some() && (mark || clear_marked || title.is_some()) {
        return Err(RmuxError::Server(
            "select-pane -U/-D/-L/-R cannot be combined with -m, -M, or -T".to_owned(),
        ));
    }
    if style.is_some() && (direction.is_some() || last || mark || clear_marked) {
        return Err(RmuxError::Server(
            "select-pane -P cannot be combined with -U, -D, -L, -R, -l, -m, or -M".to_owned(),
        ));
    }
    if last && (direction.is_some() || mark || clear_marked) {
        return Err(RmuxError::Server(
            "select-pane -l cannot be combined with -U, -D, -L, -R, -m, or -M".to_owned(),
        ));
    }

    let target = match target {
        Some(target) => target,
        None => implicit_pane_target(sessions, find_context, "select-pane")?,
    };

    if last {
        Ok(Request::LastPane(LastPaneRequest {
            target: WindowTarget::with_window(target.session_name().clone(), target.window_index()),
            preserve_zoom,
            input_disabled,
        }))
    } else if let Some(direction) = direction {
        Ok(Request::SelectPaneAdjacent(SelectPaneAdjacentRequest {
            target,
            direction,
            preserve_zoom,
        }))
    } else if mark || clear_marked {
        Ok(Request::SelectPaneMark(SelectPaneMarkRequest {
            target,
            clear: clear_marked,
            title,
        }))
    } else {
        Ok(Request::SelectPane(Box::new(SelectPaneRequest {
            target,
            title,
            style,
            input_disabled,
            preserve_zoom,
        })))
    }
}

pub(super) fn parse_split_window(
    args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    parse_split_window_command(args, sessions, find_context, false).map(|command| command.request)
}

pub(super) fn parse_queued_split_window(
    args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedSplitWindowCommand, RmuxError> {
    parse_split_window_command(args, sessions, find_context, true)
}

fn parse_split_window_command(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    allow_print_flags: bool,
) -> Result<ParsedSplitWindowCommand, RmuxError> {
    let mut direction = SplitDirection::Vertical;
    let mut direction_set = false;
    let mut before = false;
    let mut environment = Vec::new();
    let mut target = None;
    let mut start_directory: Option<std::path::PathBuf> = None;
    let mut detached = false;
    let mut size = None;
    let mut legacy_percentage = false;
    let mut full_size = false;
    let mut preserve_zoom = false;
    let mut print_target = false;
    let mut format = None;
    let mut stdin = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-h" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "split-window accepts only one of -h or -v".to_owned(),
                    ));
                }
                direction = SplitDirection::Horizontal;
                direction_set = true;
            }
            "-v" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "split-window accepts only one of -h or -v".to_owned(),
                    ));
                }
                direction = SplitDirection::Vertical;
                direction_set = true;
            }
            "-b" => {
                let _ = args.optional();
                before = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-f" => {
                let _ = args.optional();
                full_size = true;
            }
            "-Z" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-l" => {
                let _ = args.optional();
                size = Some(args.required("-l size")?);
            }
            "-p" => {
                let _ = args.optional();
                let _ = args.required("-p size")?;
                legacy_percentage = true;
            }
            token if legacy_percentage_attached_value(token) => {
                let _ = args.optional();
                legacy_percentage = true;
            }
            "-P" if allow_print_flags => {
                let _ = args.optional();
                print_target = true;
            }
            "-F" if allow_print_flags => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            "-I" => {
                let _ = args.optional();
                stdin = true;
            }
            "-F" | "-P" => {
                return Err(unsupported_flag("split-window", token));
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(std::path::PathBuf::from(
                    args.required("-c start-directory")?,
                ));
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_split_window_target(args.required("-t target")?)?);
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e name=value")?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "bdfhIPvZP", "ceFltp") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('b') => before = true,
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare('f') => full_size = true,
                        CompactFlag::Bare('h') => {
                            if direction_set {
                                return Err(RmuxError::Server(
                                    "split-window accepts only one of -h or -v".to_owned(),
                                ));
                            }
                            direction = SplitDirection::Horizontal;
                            direction_set = true;
                        }
                        CompactFlag::Bare('I') => stdin = true,
                        CompactFlag::Bare('P') if allow_print_flags => print_target = true,
                        CompactFlag::Bare('P') => {
                            return Err(unsupported_flag("split-window", "-P"));
                        }
                        CompactFlag::Bare('v') => {
                            if direction_set {
                                return Err(RmuxError::Server(
                                    "split-window accepts only one of -h or -v".to_owned(),
                                ));
                            }
                            direction = SplitDirection::Vertical;
                            direction_set = true;
                        }
                        CompactFlag::Bare('Z') => preserve_zoom = true,
                        compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                            start_directory = Some(std::path::PathBuf::from(
                                compact_flag.value_or_next(&mut args, "-c start-directory")?,
                            ));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'e', .. } => {
                            environment
                                .push(compact_flag.value_or_next(&mut args, "-e name=value")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'F', .. }
                            if allow_print_flags =>
                        {
                            format = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        CompactFlag::Value { flag: 'F', .. } => {
                            return Err(unsupported_flag("split-window", "-F"));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'l', .. } => {
                            size = Some(compact_flag.value_or_next(&mut args, "-l size")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'p', .. } => {
                            let _ = compact_flag.value_or_next(&mut args, "-p size")?;
                            legacy_percentage = true;
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_split_window_target(
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("split-window", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    let command = (!args.is_empty()).then_some(args.remaining());
    let stdin_to_empty_pane = stdin && command.is_none();
    if size.is_none() && legacy_percentage {
        return Err(RmuxError::Server("size missing".to_owned()));
    }
    let target = target.unwrap_or(implicit_split_target(
        sessions,
        find_context,
        "split-window",
    )?);

    let request = if command.is_some()
        || start_directory.is_some()
        || detached
        || size.is_some()
        || full_size
        || preserve_zoom
        || stdin_to_empty_pane
    {
        Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target,
            direction,
            before,
            environment: (!environment.is_empty()).then_some(environment),
            command,
            process_command: stdin_to_empty_pane.then(|| ProcessCommand::Shell(String::new())),
            start_directory,
            keep_alive_on_exit: stdin_to_empty_pane.then_some(true),
            detached,
            size,
            preserve_zoom,
            full_size,
            stdin_payload: stdin_to_empty_pane.then(Vec::new),
        }))
    } else {
        Request::SplitWindow(SplitWindowRequest {
            target,
            direction,
            before,
            environment: (!environment.is_empty()).then_some(environment),
        })
    };

    Ok(ParsedSplitWindowCommand {
        request,
        print_target,
        format: format.unwrap_or_else(|| DEFAULT_SPLIT_WINDOW_PRINT_FORMAT.to_owned()),
    })
}

fn legacy_percentage_attached_value(token: &str) -> bool {
    token.starts_with("-p") && token != "-p" && !token.starts_with("--")
}

pub(super) fn parse_swap_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
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
                let Some(cluster) = parse_compact_flag_cluster(token, "dDUZ", "st") else {
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
    sessions: &SessionStore,
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
                let Some(cluster) = parse_compact_flag_cluster(token, "abdP", "Fstn") else {
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

    Ok(Request::BreakPane(Box::new(BreakPaneRequest {
        source: source.unwrap_or(implicit_pane_target(sessions, find_context, "break-pane")?),
        target,
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
    sessions: &SessionStore,
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
            _ => break,
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
    sessions: &SessionStore,
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
            _ => break,
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
