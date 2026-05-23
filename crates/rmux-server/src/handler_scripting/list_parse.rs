use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    ListPanesRequest, ListSessionsRequest, ListWindowsRequest, Request, RmuxError, SessionName,
    Target,
};

use crate::pane_terminals::session_not_found;

use super::parse_session_name;
use super::tokens::CommandTokens;
use super::values::{missing_argument, unsupported_flag};

pub(super) fn parse_list_sessions(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut format = None;
    let mut filter = None;
    let mut sort_order = None;
    let mut reversed = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-F" => format = Some(args.required("-F format")?),
            "-f" => filter = Some(args.required("-f filter")?),
            "-O" => sort_order = Some(args.required("-O sort-order")?),
            "-r" => reversed = true,
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

pub(super) fn parse_list_windows(mut args: CommandTokens) -> Result<Request, RmuxError> {
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
        target: target.ok_or_else(|| missing_argument("list-windows", "-t target"))?,
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

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-t" => {
                let (session_name, window_index) =
                    parse_list_panes_target(args.required("-t target")?, sessions)?;
                target = Some(session_name);
                target_window_index = window_index;
            }
            "-F" => format = Some(args.required("-F format")?),
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

    Ok(Request::ListPanes(ListPanesRequest {
        target,
        target_window_index,
        format,
    }))
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
) -> Result<(SessionName, Option<u32>), RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => {
            let active_window = sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?
                .active_window_index();
            Ok((session_name, Some(active_window)))
        }
        Ok(Target::Window(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Ok(Target::Pane(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Err(error) => Err(error),
    }
}
