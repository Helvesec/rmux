//! Queue-time rewriting of command target arguments into resolved targets.

use std::collections::VecDeque;

use rmux_core::{
    command_target_metadata, CommandTargetSpec, ManagedSessionStore, TargetFindContext,
    TargetFindFlags, TargetFindType,
};
use rmux_proto::{RmuxError, Target, WindowTarget};

use super::new_window::new_window_target_is_session;
use super::{
    implicit_window_target, resolve_target_argument_with_spec, short_option_prefix_len,
    short_option_takes_next_value, target_spec_for_flag,
};
use crate::handler::switch_client_target_find_type;

pub(in crate::handler::scripting_support) fn resolve_queue_target_arguments(
    command_name: &str,
    arguments: Vec<String>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Vec<String>, RmuxError> {
    if !queue_target_resolution_enabled(command_name) {
        return Ok(arguments);
    }

    let mut resolved = Vec::with_capacity(arguments.len());
    let all_arguments = arguments.clone();
    let option_prefix_len = short_option_prefix_len(command_name, &arguments);
    let positional_arguments = arguments[option_prefix_len..].to_vec();
    let mut arguments = VecDeque::from(arguments[..option_prefix_len].to_vec());
    while let Some(argument) = arguments.pop_front() {
        if argument == "--" {
            resolved.push(argument);
            resolved.extend(arguments);
            break;
        }

        if let Some((bare_flags, flag, attached_value)) =
            compact_queue_target_argument_parts(command_name, &argument)
        {
            resolved.extend(bare_flags.into_iter().map(|flag| format!("-{flag}")));
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
            continue;
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
            let takes_next_value = short_option_takes_next_value(command_name, &argument);
            resolved.push(argument);
            if takes_next_value {
                if let Some(value) = arguments.pop_front() {
                    resolved.push(value);
                }
            }
        }
    }
    resolved.extend(positional_arguments);

    Ok(resolved)
}

fn resolve_queue_placement_anchor_target(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
    sessions: &ManagedSessionStore,
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

pub(super) fn preserve_link_window_bare_destination_value(
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

pub(super) fn queue_target_spec_for_flag(
    command_name: &str,
    flag: char,
    value: &str,
    arguments: &[String],
) -> Option<CommandTargetSpec> {
    let mut spec = target_spec_for_flag(command_name, flag)?;
    if command_target_metadata(command_name)
        .and_then(|metadata| metadata.source)
        .is_some_and(|source| {
            source.flag == flag && source.flags.contains(TargetFindFlags::DEFAULT_MARKED)
        })
    {
        // DEFAULT_MARKED is tmux's fallback for omitted source targets (for
        // example bare `join-pane`/`swap-pane`).  An explicit `-s` value must
        // resolve normally; otherwise the queue/source-file path silently
        // throws the user-supplied source away and falls back to `{marked}`.
        spec.flags = TargetFindFlags::NONE;
    }
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
    } else if matches!(command_name, "switch-client" | "attach-session") && flag == 't' {
        // Both commands accept a window/pane-qualified destination. Collapsing
        // attach-session to its session metadata here loses the selection before
        // the client move can resolve and commit its stable target identity.
        spec.find_type = switch_client_target_find_type(value);
    } else if command_name == "set-option" && flag == 't' {
        spec.find_type = set_option_target_find_type(arguments);
    }
    Some(spec)
}

fn set_option_target_find_type(arguments: &[String]) -> TargetFindType {
    let mut find_type = TargetFindType::Session;
    let mut global = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            index += 1;
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }
        if argument == "-t" {
            index += 2;
            continue;
        }
        if argument.starts_with("-t") && argument.len() > 2 {
            index += 1;
            continue;
        }
        for flag in argument[1..].chars() {
            match flag {
                'g' => global = true,
                'p' => return TargetFindType::Pane,
                'w' => find_type = TargetFindType::Window,
                _ => {}
            }
        }
        index += 1;
    }
    // Preserve the window qualifier before canonicalizing a known window option.
    if !global
        && find_type == TargetFindType::Session
        && arguments.get(index).is_some_and(|option| {
            matches!(
                rmux_core::default_global_scope_for_option_name(option),
                Ok(rmux_proto::OptionScopeSelector::WindowGlobal)
            )
        })
    {
        return TargetFindType::Window;
    }
    find_type
}

fn has_explicit_window_part(value: &str) -> bool {
    value
        .split_once(':')
        .map(|(_, window)| !window.is_empty())
        .unwrap_or(false)
}

/// Returns the session part of a signed window-index target (`+N`, `-N`,
/// `session:+N`, `session:-N`): `Some(None)` for a bare signed index,
/// `Some(Some(session))` for a qualified one, and `None` otherwise.
pub(in crate::handler::scripting_support) fn signed_window_target_session_part(
    raw_target: &str,
) -> Option<Option<&str>> {
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

pub(super) fn short_flag_argument_parts(argument: &str) -> Option<(char, Option<&str>)> {
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

fn compact_queue_target_argument_parts<'a>(
    command_name: &str,
    argument: &'a str,
) -> Option<(Vec<char>, char, Option<&'a str>)> {
    let bare_flags = queue_compact_bare_flags(command_name)?;
    let metadata = command_target_metadata(command_name)?;
    let target_flags = [metadata.source, metadata.target]
        .into_iter()
        .flatten()
        .map(|spec| spec.flag)
        .collect::<Vec<_>>();

    let body = argument.strip_prefix('-')?;
    if body.starts_with('-') {
        return None;
    }

    for (index, flag) in body.char_indices() {
        if index == 0 || !target_flags.contains(&flag) {
            continue;
        }
        let prefix = &body[..index];
        if !prefix
            .chars()
            .all(|prefix_flag| bare_flags.contains(prefix_flag))
        {
            continue;
        }
        let attached = &body[index + flag.len_utf8()..];
        return Some((
            prefix.chars().collect(),
            flag,
            (!attached.is_empty()).then_some(attached),
        ));
    }

    None
}

fn queue_compact_bare_flags(command_name: &str) -> Option<&'static str> {
    match command_name {
        "copy-mode" => Some("deHMSqu"),
        "join-pane" | "move-pane" => Some("bdfhv"),
        "send-keys" | "send" => Some("FHlKMRX"),
        "set-hook" => Some("agpRuw"),
        "show-hooks" => Some("gpw"),
        "split-window" => Some("bdfhIkpvZP"),
        "swap-pane" => Some("dDUZ"),
        "swap-window" => Some("d"),
        _ => None,
    }
}

fn queue_target_resolution_enabled(command_name: &str) -> bool {
    // run-shell and source-file preserve CMD_FIND_CANFAIL through command-specific
    // parsers; display-panes -t names a client rather than a command target.
    command_target_metadata(command_name).is_some()
        && !matches!(command_name, "display-panes" | "run-shell" | "source-file")
}
