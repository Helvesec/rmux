use std::collections::VecDeque;

use rmux_core::{
    command_target_metadata, CommandTargetSpec, OptionStore, SessionStore, TargetFindContext,
    TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{
    MoveWindowTarget, PaneTarget, RmuxError, SelectLayoutTarget, SessionName, SplitWindowTarget,
    Target, WindowTarget,
};

use super::values::missing_argument;

pub(super) fn resolve_queue_target_arguments(
    command_name: &str,
    arguments: Vec<String>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Vec<String>, RmuxError> {
    if !queue_target_resolution_enabled(command_name) {
        return Ok(arguments);
    }

    let mut resolved = Vec::with_capacity(arguments.len());
    let all_arguments = arguments.clone();
    let mut arguments = VecDeque::from(arguments);
    while let Some(argument) = arguments.pop_front() {
        if argument == "--" {
            resolved.push(argument);
            resolved.extend(arguments);
            break;
        }

        let Some((flag, attached_value)) = short_flag_argument_parts(&argument) else {
            resolved.push(argument);
            continue;
        };
        if target_spec_for_flag(command_name, flag).is_some() {
            let value = if let Some(value) = attached_value {
                value.to_owned()
            } else {
                let Some(value) = arguments.pop_front() else {
                    resolved.push(argument);
                    break;
                };
                value
            };
            resolved.push(format!("-{flag}"));
            let spec = queue_target_spec_for_flag(command_name, flag, &value, &all_arguments)
                .expect("prevalidated target flag must have a queue target spec");
            resolved.push(resolve_target_argument_with_spec(
                value,
                spec,
                sessions,
                find_context,
            )?);
        } else {
            resolved.push(argument);
        }
    }

    Ok(resolved)
}

pub(super) fn resolve_queue_target_argument(
    command_name: &str,
    flag: char,
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<String, RmuxError> {
    let Some(spec) = target_spec_for_flag(command_name, flag) else {
        return Ok(value);
    };
    resolve_target_argument_with_spec(value, spec, sessions, find_context)
}

pub(super) fn resolve_target_argument_with_spec(
    value: String,
    spec: CommandTargetSpec,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<String, RmuxError> {
    let target = sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(value.clone()),
        spec.find_type,
        spec.flags,
        find_context,
    )?;

    Ok(target.to_string())
}

fn target_spec_for_flag(command_name: &str, flag: char) -> Option<CommandTargetSpec> {
    let metadata = command_target_metadata(command_name)?;
    [metadata.source, metadata.target]
        .into_iter()
        .flatten()
        .find(|spec| spec.flag == flag)
}

fn queue_target_spec_for_flag(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
) -> Option<CommandTargetSpec> {
    let mut spec = target_spec_for_flag(command_name, flag)?;
    if command_name == "move-window" && flag == 't' && arguments.iter().any(|arg| arg == "-r") {
        spec.find_type = TargetFindType::Session;
        spec.flags = TargetFindFlags::QUIET;
    } else if command_name == "new-window" && flag == 't' && new_window_target_is_session(value) {
        spec.find_type = TargetFindType::Session;
        spec.flags = TargetFindFlags::NONE;
    } else if matches!(command_name, "set-hook" | "show-hooks") && flag == 't' {
        spec.find_type = hook_target_find_type(value, arguments);
    }
    Some(spec)
}

fn hook_target_find_type(value: &str, arguments: &[String]) -> TargetFindType {
    if arguments.iter().any(|arg| arg == "-p") {
        TargetFindType::Pane
    } else if arguments.iter().any(|arg| arg == "-w") || value == "." {
        TargetFindType::Window
    } else if value.starts_with('%') || value.rsplit_once('.').is_some() {
        TargetFindType::Pane
    } else if value.starts_with('@')
        || value
            .rsplit_once(':')
            .is_some_and(|(_, rest)| !rest.is_empty())
    {
        TargetFindType::Window
    } else {
        TargetFindType::Session
    }
}

pub(super) fn new_window_target_is_session(value: &str) -> bool {
    !value.contains(':')
        && !value.contains('.')
        && !value.starts_with(['@', '%', '+', '-'])
        && value.parse::<u32>().is_err()
        && !matches!(
            value,
            "!" | "^" | "$" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
        )
}

fn short_flag_argument_parts(argument: &str) -> Option<(char, Option<&str>)> {
    let mut chars = argument.chars();
    if chars.next()? != '-' {
        return None;
    }
    if chars.as_str().starts_with('-') {
        return None;
    }
    let flag = chars.next()?;
    let attached = chars.as_str();
    Some((flag, (!attached.is_empty()).then_some(attached)))
}

fn queue_target_resolution_enabled(command_name: &str) -> bool {
    matches!(
        command_name,
        "break-pane"
            | "capture-pane"
            | "display-message"
            | "display-menu"
            | "display-popup"
            | "has-session"
            | "if-shell"
            | "join-pane"
            | "kill-pane"
            | "kill-session"
            | "kill-window"
            | "last-pane"
            | "last-window"
            | "list-panes"
            | "list-windows"
            | "move-pane"
            | "move-window"
            | "new-window"
            | "next-layout"
            | "next-window"
            | "paste-buffer"
            | "pipe-pane"
            | "previous-layout"
            | "previous-window"
            | "rename-session"
            | "rename-window"
            | "resize-pane"
            | "resize-window"
            | "respawn-pane"
            | "respawn-window"
            | "rotate-window"
            | "select-layout"
            | "select-pane"
            | "send-prefix"
            | "select-window"
            | "send-keys"
            | "set-environment"
            | "set-hook"
            | "set-option"
            | "set-window-option"
            | "show-environment"
            | "show-hooks"
            | "show-options"
            | "show-window-options"
            | "split-window"
            | "swap-pane"
            | "swap-window"
            | "switch-client"
    )
}

pub(super) fn queue_target_find_context(
    sessions: &SessionStore,
    options: &OptionStore,
    requester_pid: u32,
    attached_session: Option<&SessionName>,
    current_target: Option<&Target>,
    mouse_target: Option<&Target>,
    marked_target: Option<&PaneTarget>,
) -> TargetFindContext {
    let context = if let Some(current_target) = current_target {
        TargetFindContext::from_target(current_target.clone())
    } else if let Some(client_target) = client_rmux_pane_target(sessions, requester_pid) {
        TargetFindContext::from_target(client_target)
    } else {
        let current = attached_session
            .and_then(|session_name| active_session_target(sessions, session_name))
            .or_else(|| latest_detached_session_target(sessions));
        TargetFindContext::new(current)
    };

    let context = context
        .with_mouse_target(mouse_target.cloned())
        .with_marked_target(marked_target.cloned().map(Target::Pane));
    crate::handler::with_visible_pane_bases(context, sessions, options)
}

fn latest_detached_session_target(sessions: &SessionStore) -> Option<Target> {
    sessions
        .iter()
        .max_by(|(left_name, left_session), (right_name, right_session)| {
            left_session
                .activity_at()
                .cmp(&right_session.activity_at())
                .then(left_session.created_at().cmp(&right_session.created_at()))
                .then(left_session.id().cmp(&right_session.id()))
                .then(right_name.as_str().cmp(left_name.as_str()))
        })
        .and_then(|(session_name, _)| active_session_target(sessions, session_name))
}

fn client_rmux_pane_target(sessions: &SessionStore, requester_pid: u32) -> Option<Target> {
    if requester_pid == std::process::id() {
        return None;
    }

    let environment = rmux_os::process::environment(requester_pid)?;
    let rmux_pane = environment.get("RMUX_PANE")?;
    let pane_id = rmux_pane.strip_prefix('%')?.parse::<u32>().ok()?;

    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new(format!("%{pane_id}")),
            TargetFindType::Pane,
            TargetFindFlags::CANFAIL,
            &TargetFindContext::new(None),
        )
        .ok()
}

pub(super) fn active_session_target(
    sessions: &SessionStore,
    session_name: &SessionName,
) -> Option<Target> {
    let session = sessions.session(session_name)?;
    let window_index = session.active_window_index();
    let pane_index = session
        .window_at(window_index)
        .and_then(rmux_core::Window::active_pane)
        .map(rmux_core::Pane::index)?;
    Some(Target::Pane(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane_index,
    )))
}

fn implicit_target_for_type(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    find_type: TargetFindType,
    command_name: &str,
) -> Result<Target, RmuxError> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::none(),
            find_type,
            TargetFindFlags::NONE,
            find_context,
        )
        .map_err(|_| missing_argument(command_name, "-t target"))
}

pub(super) fn implicit_session_name(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<SessionName, RmuxError> {
    match implicit_target_for_type(
        sessions,
        find_context,
        TargetFindType::Session,
        command_name,
    )? {
        Target::Session(session_name) => Ok(session_name),
        _ => unreachable!("session target lookup must return a session"),
    }
}

pub(super) fn implicit_window_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<WindowTarget, RmuxError> {
    match implicit_target_for_type(sessions, find_context, TargetFindType::Window, command_name)? {
        Target::Window(target) => Ok(target),
        _ => unreachable!("window target lookup must return a window"),
    }
}

pub(super) fn implicit_pane_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<PaneTarget, RmuxError> {
    match implicit_target_for_type(sessions, find_context, TargetFindType::Pane, command_name)? {
        Target::Pane(target) => Ok(target),
        _ => unreachable!("pane target lookup must return a pane"),
    }
}

pub(super) fn marked_pane_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<PaneTarget, RmuxError> {
    sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new("{marked}"),
            TargetFindType::Pane,
            TargetFindFlags::NONE,
            find_context,
        )
        .map(|target| match target {
            Target::Pane(target) => target,
            _ => unreachable!("marked pane target lookup must return a pane"),
        })
        .map_err(|error| {
            RmuxError::Server(format!("{command_name} requires a marked pane: {error}"))
        })
}

pub(super) fn implicit_split_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    command_name: &str,
) -> Result<SplitWindowTarget, RmuxError> {
    let target = implicit_pane_target(sessions, find_context, command_name)?;
    Ok(SplitWindowTarget::Pane(target))
}

pub(super) fn parse_session_name(value: String) -> Result<SessionName, RmuxError> {
    SessionName::new(value)
}

fn parse_new_window_target(value: String) -> Result<(SessionName, Option<u32>), RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok((session_name, None)),
        Ok(Target::Window(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Ok(Target::Pane(_)) => Err(RmuxError::Server(format!(
            "invalid new-window target '{value}': new-window target must match 'session' or 'session:window'"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_new_window_target_argument(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<(SessionName, Option<u32>), RmuxError> {
    if let Some((session_name, window_part)) = value.split_once(':') {
        if window_part.is_empty() {
            return Ok((parse_session_name(session_name.to_owned())?, None));
        }
    }

    if new_window_target_is_session(&value) {
        let resolved = resolve_target_argument_with_spec(
            value,
            CommandTargetSpec {
                flag: 't',
                find_type: TargetFindType::Session,
                flags: TargetFindFlags::NONE,
            },
            sessions,
            find_context,
        )?;
        return parse_new_window_target(resolved);
    }

    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok((session_name, None)),
        Ok(Target::Window(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Ok(Target::Pane(_)) | Err(_) => {
            let resolved =
                resolve_queue_target_argument("new-window", 't', value, sessions, find_context)?;
            parse_new_window_target(resolved)
        }
    }
}

pub(super) fn parse_target_arg(command: &str, value: String) -> Result<Target, RmuxError> {
    Target::parse(&value)
        .map_err(|error| RmuxError::Server(format!("invalid {command} target '{value}': {error}")))
}

pub(super) fn parse_window_target(command: &str, value: String) -> Result<WindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Window(target)) => Ok(target),
        Ok(_) => Err(RmuxError::Server(format!(
            "{command} target must match 'session:window'"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_pane_target(_command: &str, value: String) -> Result<PaneTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Pane(target)) => Ok(target),
        Ok(Target::Window(target)) => Ok(PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            0,
        )),
        Ok(Target::Session(session_name)) => Ok(PaneTarget::with_window(session_name, 0, 0)),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_move_window_target(value: String) -> Result<MoveWindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(MoveWindowTarget::Session(session_name)),
        Ok(Target::Window(target)) => Ok(MoveWindowTarget::Window(target)),
        Ok(Target::Pane(_)) => Err(RmuxError::Server(format!(
            "invalid move-window target '{value}': move-window target must match 'session' or 'session:window'"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_split_window_target(value: String) -> Result<SplitWindowTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(SplitWindowTarget::Session(session_name)),
        Ok(Target::Pane(target)) => Ok(SplitWindowTarget::Pane(target)),
        Ok(Target::Window(_)) => Err(RmuxError::Server(format!(
            "invalid split-window target '{value}': split-window requires a session or pane target"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_select_layout_target(value: String) -> Result<SelectLayoutTarget, RmuxError> {
    match Target::parse(&value) {
        Ok(Target::Session(session_name)) => Ok(SelectLayoutTarget::Session(session_name)),
        Ok(Target::Window(target)) => Ok(SelectLayoutTarget::Window(target)),
        Ok(Target::Pane(_)) => Err(RmuxError::Server(format!(
            "invalid select-layout target '{value}': select-layout requires a session or window target"
        ))),
        Err(error) => Err(error),
    }
}

pub(super) fn parse_layout_name(value: &str) -> Result<rmux_proto::LayoutName, RmuxError> {
    value.parse()
}

pub(super) fn is_unsupported_named_layout(layout: rmux_proto::LayoutName) -> bool {
    matches!(
        layout,
        rmux_proto::LayoutName::MainHorizontalMirrored
            | rmux_proto::LayoutName::MainVerticalMirrored
    )
}
