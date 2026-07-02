use rmux_core::{
    SessionStore, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{
    ListPanesRequest, ListSessionsRequest, ListWindowsRequest, Request, RmuxError, SessionName,
    Target,
};

use crate::pane_terminals::session_not_found;

use super::tokens::CommandTokens;
use super::values::{missing_argument, unsupported_flag};
use super::{implicit_session_name, parse_session_name};

pub(super) fn parse_list_sessions(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut format = None;
    let mut filter = None;
    let sort_order = None;
    let reversed = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-F" => format = Some(args.required("-F format")?),
            "-f" => filter = Some(args.required("-f filter")?),
            "-O" => return Err(unsupported_flag("list-sessions", "-O")),
            "-r" => return Err(unsupported_flag("list-sessions", "-r")),
            flag if flag.starts_with('-') => return Err(unsupported_flag("list-sessions", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for list-sessions"
                )));
            }
        }
    }

    Ok(Request::ListSessions(ListSessionsRequest {
        format,
        filter,
        sort_order,
        reversed,
    }))
}

pub(super) fn parse_list_windows(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut format = None;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-t" => target = Some(parse_session_name(args.required("-t target")?)?),
            "-F" => format = Some(args.required("-F format")?),
            flag if flag.starts_with('-') => return Err(unsupported_flag("list-windows", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for list-windows"
                )));
            }
        }
    }

    Ok(Request::ListWindows(ListWindowsRequest {
        target: target.unwrap_or(implicit_session_name(
            sessions,
            find_context,
            "list-windows",
        )?),
        format,
    }))
}

pub(super) fn parse_list_panes(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut target_window_index = None;
    let mut format = None;
    let mut session_scope = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-t" => {
                let (session_name, window_index) =
                    parse_list_panes_target(args.required("-t target")?, sessions, find_context)?;
                target = Some(session_name);
                target_window_index = window_index;
            }
            "-F" => format = Some(args.required("-F format")?),
            "-s" => session_scope = true,
            flag if flag.starts_with('-') => return Err(unsupported_flag("list-panes", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for list-panes"
                )));
            }
        }
    }

    let (target, target_window_index) = match target {
        Some(target) => (target, target_window_index),
        None => implicit_list_panes_target(sessions, find_context)?,
    };
    let target_window_index = if session_scope {
        None
    } else {
        target_window_index
    };

    Ok(Request::ListPanes(ListPanesRequest {
        target,
        target_window_index,
        format,
    }))
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct ParsedListPanesAllCommand {
    pub(in crate::handler) format: Option<String>,
}

pub(super) fn parse_queued_list_panes_all(
    mut args: CommandTokens,
) -> Result<Option<ParsedListPanesAllCommand>, RmuxError> {
    let mut all_sessions = false;
    let mut format = None;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-a" => all_sessions = true,
            "-F" => format = Some(args.required("-F format")?),
            flag if flag.starts_with('-') && compact_list_panes_all_flags(flag) => {
                all_sessions = true;
            }
            flag if flag.starts_with('-') => return Ok(None),
            _ => return Ok(None),
        }
    }

    Ok(all_sessions.then_some(ParsedListPanesAllCommand { format }))
}

fn compact_list_panes_all_flags(flag: &str) -> bool {
    let Some(rest) = flag.strip_prefix('-') else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|ch| matches!(ch, 'a' | 's')) && rest.contains('a')
}

fn implicit_list_panes_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<(SessionName, Option<u32>), RmuxError> {
    match find_context.current() {
        Some(Target::Session(session_name)) => {
            let active_window = sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?
                .active_window_index();
            Ok((session_name.clone(), Some(active_window)))
        }
        Some(Target::Window(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Some(Target::Pane(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        None => Err(missing_argument("list-panes", "-t target")),
    }
}

fn parse_list_panes_target(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<(SessionName, Option<u32>), RmuxError> {
    match sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(value),
        TargetFindType::Window,
        TargetFindFlags::NONE,
        find_context,
    )? {
        Target::Session(session_name) => {
            let active_window = sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?
                .active_window_index();
            Ok((session_name, Some(active_window)))
        }
        Target::Window(target) => Ok((target.session_name().clone(), Some(target.window_index()))),
        Target::Pane(target) => Ok((target.session_name().clone(), Some(target.window_index()))),
    }
}
