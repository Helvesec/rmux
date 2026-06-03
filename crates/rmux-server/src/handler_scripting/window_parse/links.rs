use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, Request, RmuxError, SwapWindowRequest,
    UnlinkWindowRequest, WindowTarget,
};

use super::super::tokens::CommandTokens;
use super::super::values::{missing_argument, unsupported_flag};
use super::super::{
    implicit_session_name, marked_pane_target, parse_move_window_target, parse_window_target,
};

pub(in crate::handler::scripting_support) fn parse_move_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut renumber = false;
    let mut kill_destination = false;
    let mut detached = false;
    let mut source = None;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-r" => {
                let _ = args.optional();
                renumber = true;
            }
            "-k" => {
                let _ = args.optional();
                kill_destination = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "move-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_move_window_target(args.required("-t target")?)?);
            }
            _ => break,
        }
    }
    args.no_extra("move-window")?;

    if renumber {
        if source.is_some() {
            return Err(RmuxError::Server(
                "move-window -r does not accept -s".to_owned(),
            ));
        }
        if kill_destination {
            return Err(RmuxError::Server(
                "move-window -r does not accept -k".to_owned(),
            ));
        }
        match target {
            Some(MoveWindowTarget::Session(_)) => {}
            Some(MoveWindowTarget::Window(_)) => {
                return Err(RmuxError::Server(
                    "move-window -r requires a session target".to_owned(),
                ));
            }
            None => {
                target = Some(MoveWindowTarget::Session(implicit_session_name(
                    sessions,
                    find_context,
                    "move-window",
                )?));
            }
        }
    } else if source.is_none() || !matches!(target, Some(MoveWindowTarget::Window(_))) {
        return Err(RmuxError::Server(
            "move-window requires -s source-window and -t destination-window targets".to_owned(),
        ));
    }

    Ok(Request::MoveWindow(MoveWindowRequest {
        source,
        target: target.expect("validated move-window target"),
        renumber,
        kill_destination,
        detached,
    }))
}

pub(in crate::handler::scripting_support) fn parse_link_window(
    mut args: CommandTokens,
) -> Result<Request, RmuxError> {
    let mut after = false;
    let mut before = false;
    let mut detached = false;
    let mut kill_destination = false;
    let mut source = None;
    let mut target = None;

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
                        "link-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                if after {
                    return Err(RmuxError::Server(
                        "link-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                before = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-k" => {
                let _ = args.optional();
                kill_destination = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "link-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "link-window",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }
    args.no_extra("link-window")?;

    Ok(Request::LinkWindow(LinkWindowRequest {
        source: source.ok_or_else(|| missing_argument("link-window", "-s target"))?,
        target: target.ok_or_else(|| missing_argument("link-window", "-t target"))?,
        after,
        before,
        kill_destination,
        detached,
    }))
}

pub(in crate::handler::scripting_support) fn parse_swap_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut detached = false;
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
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "swap-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "swap-window",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }
    args.no_extra("swap-window")?;

    let source = match source {
        Some(source) => source,
        None => {
            let marked = marked_pane_target(sessions, find_context, "swap-window")?;
            WindowTarget::with_window(marked.session_name().clone(), marked.window_index())
        }
    };

    Ok(Request::SwapWindow(SwapWindowRequest {
        source,
        target: target.ok_or_else(|| missing_argument("swap-window", "-t target"))?,
        detached,
    }))
}

pub(in crate::handler::scripting_support) fn parse_unlink_window(
    mut args: CommandTokens,
) -> Result<Request, RmuxError> {
    let mut kill_if_last = false;
    let mut target = None;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-k" => kill_if_last = true,
            "-t" => {
                target = Some(parse_window_target(
                    "unlink-window",
                    args.required("-t target")?,
                )?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("unlink-window", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for unlink-window"
                )));
            }
        }
    }

    Ok(Request::UnlinkWindow(UnlinkWindowRequest {
        target: target.ok_or_else(|| missing_argument("unlink-window", "-t target"))?,
        kill_if_last,
    }))
}
