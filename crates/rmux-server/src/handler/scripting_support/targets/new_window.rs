//! new-window target parsing, including relative window offsets.

use rmux_core::{
    CommandTargetSpec, ManagedSessionStore, TargetFindContext, TargetFindFlags, TargetFindType,
};
use rmux_proto::{RmuxError, SessionName, Target};

use super::super::values::parse_u32;
use super::{
    implicit_session_name, resolve_queue_target_argument, resolve_target_argument_with_spec,
};

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
pub(in crate::handler::scripting_support) enum NewWindowTargetIndex {
    Absolute(u32),
    Relative(i32),
}

impl NewWindowTargetIndex {
    pub(in crate::handler::scripting_support) fn checked_add_one(self) -> Result<Self, RmuxError> {
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

pub(in crate::handler::scripting_support) fn parse_queued_new_window_target_argument(
    value: String,
    sessions: &ManagedSessionStore,
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

pub(in crate::handler::scripting_support) fn parse_new_window_target_argument(
    value: String,
    sessions: &ManagedSessionStore,
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
        Ok(Target::Window(target)) => {
            Ok((target.session_name().clone(), Some(target.window_index())))
        }
        Ok(Target::Session(_)) | Ok(Target::Pane(_)) | Err(_) => {
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
