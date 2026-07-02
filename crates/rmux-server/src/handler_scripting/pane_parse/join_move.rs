use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    JoinPaneRequest, MovePaneRequest, PaneSplitSize, Request, RmuxError, SplitDirection,
};

use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::{missing_argument, parse_percentage, parse_u32, unsupported_flag};
use super::super::{marked_pane_target_or_current, parse_pane_target};

pub(in crate::handler::scripting_support) fn parse_join_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    parse_join_or_move_pane(&mut args, "join-pane", false, sessions, find_context)
}

pub(in crate::handler::scripting_support) fn parse_move_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    parse_join_or_move_pane(&mut args, "move-pane", true, sessions, find_context)
}

fn parse_join_or_move_pane(
    args: &mut CommandTokens,
    command: &str,
    as_move: bool,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut detached = false;
    let mut before = false;
    let mut full_size = false;
    let mut direction = SplitDirection::Vertical;
    let mut size = None;
    let mut legacy_percentage = false;
    let mut source = None;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
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
            "-h" => {
                let _ = args.optional();
                direction = SplitDirection::Horizontal;
            }
            "-l" => {
                let _ = args.optional();
                if size.is_some() {
                    return Err(RmuxError::Server(format!("{command} accepts only one -l")));
                }
                size = Some(parse_pane_split_size(
                    command,
                    "-l",
                    &args.required("-l size")?,
                )?);
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
            "-v" => {
                let _ = args.optional();
                if direction != SplitDirection::Horizontal {
                    direction = SplitDirection::Vertical;
                }
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_pane_target(command, args.required("-s target")?)?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(command, args.required("-t target")?)?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "bdfhv", "lpst") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('b') => before = true,
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare('f') => full_size = true,
                        CompactFlag::Bare('h') => {
                            direction = SplitDirection::Horizontal;
                        }
                        CompactFlag::Bare('v') => {
                            if direction != SplitDirection::Horizontal {
                                direction = SplitDirection::Vertical;
                            }
                        }
                        compact_flag @ CompactFlag::Value { flag: 'l', .. } => {
                            if size.is_some() {
                                return Err(RmuxError::Server(format!(
                                    "{command} accepts only one -l"
                                )));
                            }
                            size = Some(parse_pane_split_size(
                                command,
                                "-l",
                                &compact_flag.value_or_next(args, "-l size")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'p', .. } => {
                            let _ = compact_flag.value_or_next(args, "-p size")?;
                            legacy_percentage = true;
                        }
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            source = Some(parse_pane_target(
                                command,
                                compact_flag.value_or_next(args, "-s target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                command,
                                compact_flag.value_or_next(args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag(command, &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    args.no_extra(command)?;
    if size.is_none() && legacy_percentage {
        // tmux treats the legacy -p argument as a modifier for -l, not as a
        // size source by itself.
        return Err(RmuxError::Server("size missing".to_owned()));
    }

    let source = match source {
        Some(source) => source,
        None => marked_pane_target_or_current(sessions, find_context, command)?,
    };

    let request = JoinPaneRequest {
        source,
        target: target.ok_or_else(|| missing_argument(command, "-t target"))?,
        direction,
        detached,
        before,
        full_size,
        size,
    };

    if as_move {
        Ok(Request::MovePane(MovePaneRequest {
            source: request.source,
            target: request.target,
            direction: request.direction,
            detached: request.detached,
            before: request.before,
            full_size: request.full_size,
            size: request.size,
        }))
    } else {
        Ok(Request::JoinPane(request))
    }
}

fn legacy_percentage_attached_value(token: &str) -> bool {
    token.starts_with("-p") && token != "-p" && !token.starts_with("--")
}

fn parse_pane_split_size(
    command: &str,
    flag: &str,
    value: &str,
) -> Result<PaneSplitSize, RmuxError> {
    if let Some(percentage) = value.strip_suffix('%') {
        return Ok(PaneSplitSize::Percentage(parse_percentage(
            command, flag, percentage,
        )?));
    }

    Ok(PaneSplitSize::Absolute(parse_u32(command, flag, value)?))
}
