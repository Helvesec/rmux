use std::collections::VecDeque;

use rmux_core::{
    command_target_metadata, CommandTargetSpec, OptionStore, SessionStore, TargetFindContext,
    TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{
    MoveWindowTarget, PaneTarget, RmuxError, SelectLayoutTarget, SessionName, SplitWindowTarget,
    Target, WindowTarget,
};

use super::super::target_support::pane_id_target;
use super::values::{missing_argument, parse_u32};

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
            if let Some(anchor) = resolve_queue_placement_anchor_target(
                command_name,
                flag,
                &value,
                &all_arguments,
                sessions,
                find_context,
            )? {
                resolved.push(anchor);
                continue;
            }
            if let Some(session_value) =
                queue_session_destination_value(command_name, flag, &value, &all_arguments)
            {
                let session = resolve_target_argument_with_spec(
                    session_value,
                    CommandTargetSpec {
                        flag,
                        find_type: TargetFindType::Session,
                        flags: TargetFindFlags::NONE,
                    },
                    sessions,
                    find_context,
                )?;
                resolved.push(format!("{session}:"));
                continue;
            }
            if preserve_link_window_bare_destination_value(
                command_name,
                flag,
                &value,
                &all_arguments,
            ) {
                resolved.push(value);
                continue;
            }
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

fn resolve_queue_placement_anchor_target(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Option<String>, RmuxError> {
    if !link_or_move_window_command(command_name)
        || flag != 't'
        || !arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "-a" | "-b"))
    {
        return Ok(None);
    }
    let Some(session_value) = signed_window_target_session_part(value) else {
        return Ok(None);
    };
    let target = if let Some(session_value) = session_value {
        let resolved = resolve_target_argument_with_spec(
            session_value.to_owned(),
            CommandTargetSpec {
                flag,
                find_type: TargetFindType::Session,
                flags: TargetFindFlags::NONE,
            },
            sessions,
            find_context,
        )?;
        let Target::Session(session_name) = Target::parse(&resolved)? else {
            unreachable!("session target lookup must return a session");
        };
        let window_index = sessions
            .session(&session_name)
            .ok_or_else(|| crate::pane_terminals::session_not_found(&session_name))?
            .active_window_index();
        WindowTarget::with_window(session_name, window_index)
    } else {
        implicit_window_target(sessions, find_context, command_name)?
    };
    Ok(Some(target.to_string()))
}

fn queue_session_destination_value(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
) -> Option<String> {
    if flag != 't' {
        return None;
    }
    if !link_or_move_window_command(command_name) {
        return None;
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-a" | "-b"))
    {
        return None;
    }
    if command_name == "move-window" && arguments.iter().any(|argument| argument == "-r") {
        return None;
    }
    session_only_window_destination_value(value)
}

fn session_only_window_destination_value(value: &str) -> Option<String> {
    if let Some(session) = value
        .strip_suffix(':')
        .filter(|session| !session.is_empty())
    {
        return Some(session.to_owned());
    }
    None
}

fn preserve_link_window_bare_destination_value(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
) -> bool {
    if flag != 't' {
        return false;
    }
    if !link_or_move_window_command(command_name) {
        return false;
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-a" | "-b"))
    {
        return false;
    }
    if command_name == "move-window" && arguments.iter().any(|argument| argument == "-r") {
        return false;
    }
    bare_link_window_destination_candidate(value)
}

pub(in crate::handler::scripting_support) fn bare_link_window_destination_candidate(
    value: &str,
) -> bool {
    !value.is_empty()
        && !value.contains([':', '.'])
        && !value.starts_with(['@', '%', '+', '-', '='])
        && value.parse::<u32>().is_err()
        && !matches!(
            value,
            "!" | "^" | "$" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
        )
}

fn link_or_move_window_command(command_name: &str) -> bool {
    matches!(
        command_name,
        "link-window" | "linkw" | "move-window" | "movew"
    )
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
    if command_name == "move-window"
        && flag == 't'
        && arguments.iter().any(|arg| arg == "-r")
        && !has_explicit_window_part(value)
    {
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

fn has_explicit_window_part(value: &str) -> bool {
    value
        .split_once(':')
        .map(|(_, window)| !window.is_empty())
        .unwrap_or(false)
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
        "attach-session"
            | "break-pane"
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
            | "link-window"
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

pub(super) struct QueueTargetFindContextInput<'a> {
    pub(super) sessions: &'a SessionStore,
    pub(super) options: &'a OptionStore,
    pub(super) requester_pane_id: Option<u32>,
    pub(super) attached_session: Option<&'a SessionName>,
    pub(super) current_target: Option<&'a Target>,
    pub(super) mouse_target: Option<&'a Target>,
    pub(super) marked_target: Option<&'a PaneTarget>,
}

pub(super) fn queue_target_find_context(
    input: QueueTargetFindContextInput<'_>,
) -> TargetFindContext {
    let context = if let Some(current_target) = input.current_target {
        TargetFindContext::from_target(current_target.clone())
    } else {
        let current = input
            .requester_pane_id
            .and_then(|pane_id| pane_id_target(input.sessions, pane_id))
            .or_else(|| {
                input
                    .attached_session
                    .and_then(|session_name| active_session_target(input.sessions, session_name))
            })
            .or_else(|| latest_detached_session_target(input.sessions));
        TargetFindContext::new(current)
    };

    let context = context
        .with_mouse_target(input.mouse_target.cloned())
        .with_marked_target(input.marked_target.cloned().map(Target::Pane));
    crate::handler::with_visible_pane_bases(context, input.sessions, input.options)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NewWindowTargetIndex {
    Absolute(u32),
    Relative(i32),
}

impl NewWindowTargetIndex {
    pub(super) fn checked_add_one(self) -> Result<Self, RmuxError> {
        match self {
            Self::Absolute(index) => Ok(Self::Absolute(index.checked_add(1).ok_or_else(|| {
                RmuxError::Server("window index space exhausted for new-window".to_owned())
            })?)),
            Self::Relative(offset) => {
                Ok(Self::Relative(offset.checked_add(1).ok_or_else(|| {
                    RmuxError::Server("window offset space exhausted for new-window".to_owned())
                })?))
            }
        }
    }
}

pub(super) fn parse_queued_new_window_target_argument(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<(SessionName, Option<NewWindowTargetIndex>), RmuxError> {
    if let Some((session_name, window_part)) = value.split_once(':') {
        if !session_name.is_empty() {
            if let Some(offset) = parse_new_window_relative_offset(window_part)? {
                let resolved = resolve_target_argument_with_spec(
                    session_name.to_owned(),
                    CommandTargetSpec {
                        flag: 't',
                        find_type: TargetFindType::Session,
                        flags: TargetFindFlags::NONE,
                    },
                    sessions,
                    find_context,
                )?;
                let (resolved_session, _) = parse_new_window_target(resolved)?;
                return Ok((
                    resolved_session,
                    Some(NewWindowTargetIndex::Relative(offset)),
                ));
            }
        }
    } else if let Some(offset) = parse_new_window_relative_offset(&value)? {
        let session_name = implicit_session_name(sessions, find_context, "new-window")?;
        return Ok((session_name, Some(NewWindowTargetIndex::Relative(offset))));
    }

    let (session_name, window_index) =
        parse_new_window_target_argument(value, sessions, find_context)?;
    Ok((
        session_name,
        window_index.map(NewWindowTargetIndex::Absolute),
    ))
}

pub(super) fn parse_new_window_target_argument(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<(SessionName, Option<u32>), RmuxError> {
    if let Some((session_name, window_part)) = value.split_once(':') {
        if !session_name.is_empty() && new_window_window_part_is_absolute(window_part) {
            let resolved = resolve_target_argument_with_spec(
                session_name.to_owned(),
                CommandTargetSpec {
                    flag: 't',
                    find_type: TargetFindType::Session,
                    flags: TargetFindFlags::NONE,
                },
                sessions,
                find_context,
            )?;
            let (resolved_session, _) = parse_new_window_target(resolved)?;
            let window_index = if window_part.is_empty() {
                None
            } else {
                Some(parse_u32("new-window", "target-window", window_part)?)
            };
            return Ok((resolved_session, window_index));
        }
        if new_window_window_part_is_relative(window_part) {
            let resolved =
                resolve_queue_target_argument("new-window", 't', value, sessions, find_context)?;
            return parse_new_window_target(resolved);
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

fn new_window_window_part_is_absolute(window_part: &str) -> bool {
    window_part.is_empty() || window_part.bytes().all(|byte| byte.is_ascii_digit())
}

fn new_window_window_part_is_relative(window_part: &str) -> bool {
    let Some(rest) = window_part.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_new_window_relative_offset(value: &str) -> Result<Option<i32>, RmuxError> {
    let Some(sign) = value
        .chars()
        .next()
        .filter(|sign| matches!(sign, '+' | '-'))
    else {
        return Ok(None);
    };
    let rest = &value[sign.len_utf8()..];
    if !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let magnitude = if rest.is_empty() {
        1
    } else {
        rest.parse::<i32>()
            .map_err(|_| RmuxError::invalid_target(value, "offset must fit in a signed integer"))?
    };
    Ok(Some(if sign == '+' { magnitude } else { -magnitude }))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn session_store_with_alpha_beta() -> SessionStore {
        let mut sessions = SessionStore::new();
        for name in ["alpha", "beta"] {
            sessions
                .create_session(
                    session_name(name),
                    rmux_proto::TerminalSize { cols: 80, rows: 24 },
                )
                .expect("session create succeeds");
        }
        sessions
    }

    #[test]
    fn queue_keeps_bare_link_window_destination_unresolved() {
        let sessions = session_store_with_alpha_beta();
        let resolved = resolve_queue_target_arguments(
            "link-window",
            vec![
                "-s".to_owned(),
                "alpha:0".to_owned(),
                "-t".to_owned(),
                "beta".to_owned(),
            ],
            &sessions,
            &TargetFindContext::new(Some(Target::Pane(PaneTarget::with_window(
                session_name("alpha"),
                0,
                0,
            )))),
        )
        .expect("queue targets resolve");

        assert_eq!(resolved, ["-s", "alpha:0", "-t", "beta"]);
    }
}
