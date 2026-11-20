use std::collections::BTreeSet;

use rmux_client::Connection;
use rmux_proto::OptionScopeSelector;

use super::super::{
    expect_command_output, resolve_current_pane_target, resolve_session_target_or_current,
    resolve_session_target_spec, resolve_window_index_target_or_current_session,
    resolve_window_target_or_current, resolve_window_target_spec, ExitFailure,
};
use super::LIST_WINDOWS_FILTER_SEPARATOR;
use crate::cli_args::TargetSpec;

pub(in crate::cli) fn resolve_link_window_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    after: bool,
    before: bool,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    let Some(target) = target else {
        let session_name = resolve_session_target_or_current(connection, None, command_name)?;
        if after || before {
            // tmux 3.7b anchors on the current session's active window, not
            // on the window of the pane the command runs from.
            let index = resolve_active_window_index(connection, &session_name, command_name)?;
            return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
        }
        let index = first_available_window_index(connection, &session_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    };

    if after || before {
        return resolve_window_placement_anchor_target(connection, Some(target), command_name);
    }
    resolve_window_destination_target(connection, target, command_name)
}

pub(super) fn link_target_is_explicit_session_only(target: &TargetSpec) -> bool {
    let raw = target.raw();
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if matches!(parse_bare_relative_window_offset(raw), Ok(Some(_))) {
        return false;
    }
    if is_special_window_token(raw) {
        return false;
    }
    if raw
        .strip_suffix(':')
        .is_some_and(|session| !session.is_empty())
    {
        return true;
    }
    raw.starts_with('=') && matches!(target.exact(), Some(rmux_proto::Target::Session(_)))
}

pub(super) fn is_special_window_token(raw: &str) -> bool {
    matches!(
        raw,
        "^" | "!" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
    )
}

pub(super) fn resolve_window_destination_target(
    connection: &mut Connection,
    target: &TargetSpec,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    if let Some(target) =
        resolve_bare_relative_window_target(connection, target.raw(), command_name)?
    {
        return Ok(target);
    }
    if let Some(index) = parse_bare_window_index(target.raw())? {
        let session_name = resolve_session_target_or_current(connection, None, command_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    }
    if let Some(rmux_proto::Target::Window(target)) = target.exact() {
        return Ok(target.clone());
    }
    if link_target_is_explicit_session_only(target) {
        let session_name = resolve_session_only_destination(connection, target)?;
        let index = first_available_window_index(connection, &session_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(session_name, index));
    }
    if let Some(target) =
        resolve_exact_current_window_name_destination(connection, target.raw(), command_name)?
    {
        return Ok(target);
    }
    if let Some(target) = resolve_bare_session_window_destination(connection, target)? {
        return Ok(target);
    }

    resolve_window_index_target_or_current_session(connection, Some(target), command_name)
}

fn resolve_exact_current_window_name_destination(
    connection: &mut Connection,
    raw_target: &str,
    command_name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    if !link_target_is_bare_session_candidate(raw_target) {
        return Ok(None);
    }
    let session_name = resolve_session_target_or_current(connection, None, command_name)?;
    find_window_by_name(connection, &session_name, raw_target)
}

fn resolve_bare_session_window_destination(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    if !link_target_is_bare_session_candidate(target.raw()) {
        return Ok(None);
    }
    let session_name = match resolve_session_target_spec(connection, target, false) {
        Ok(session_name) => session_name,
        Err(_) => return Ok(None),
    };
    let index = first_available_window_index(connection, &session_name)?;
    Ok(Some(rmux_proto::WindowTarget::with_window(
        session_name,
        index,
    )))
}

fn link_target_is_bare_session_candidate(raw: &str) -> bool {
    !raw.is_empty()
        && !raw.contains([':', '.'])
        && !raw.starts_with(['@', '%', '+', '-', '='])
        && raw.parse::<u32>().is_err()
        && !matches!(
            raw,
            "!" | "^" | "$" | "{start}" | "{last}" | "{end}" | "{next}" | "{previous}"
        )
}

pub(super) fn resolve_bare_relative_window_target(
    connection: &mut Connection,
    raw_target: &str,
    command_name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let Some(offset) = parse_bare_relative_window_offset(raw_target)? else {
        return Ok(None);
    };
    let current = resolve_window_target_or_current(connection, None, command_name)?;
    let index = apply_window_index_offset(current.window_index(), offset)?;
    Ok(Some(rmux_proto::WindowTarget::with_window(
        current.session_name().clone(),
        index,
    )))
}

pub(super) fn resolve_window_placement_anchor_target(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    let Some(target) = target else {
        let pane = resolve_current_pane_target(connection, command_name)?;
        return Ok(rmux_proto::WindowTarget::with_window(
            pane.session_name().clone(),
            pane.window_index(),
        ));
    };

    if let Some(session_target) = signed_window_target_session_part(target.raw()) {
        if let Some(session_target) = session_target {
            let session_target = crate::cli_args::parse_target_spec(session_target)
                .map_err(|error| ExitFailure::new(1, error))?;
            let session_name = resolve_session_target_spec(connection, &session_target, false)?;
            let window_index =
                resolve_active_window_index(connection, &session_name, command_name)?;
            return Ok(rmux_proto::WindowTarget::with_window(
                session_name,
                window_index,
            ));
        }
        return resolve_window_target_or_current(connection, None, command_name);
    }

    resolve_window_target_spec(connection, target, false)
}

fn resolve_active_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
    command_name: &str,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some("#{window_index}:#{window_active}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let stdout = String::from_utf8_lossy(output.stdout());
    for line in stdout.lines() {
        let Some((index, active)) = line.split_once(':') else {
            continue;
        };
        if active == "1" {
            return index.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("{command_name}: invalid active window: {error}"))
            });
        }
    }
    Err(ExitFailure::new(
        1,
        format!("{command_name}: no active window in session {session_name}"),
    ))
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

fn parse_bare_relative_window_offset(value: &str) -> Result<Option<i64>, ExitFailure> {
    let Some(sign) = value.as_bytes().first().copied() else {
        return Ok(None);
    };
    if !matches!(sign, b'+' | b'-') {
        return Ok(None);
    }
    let rest = &value[1..];
    let magnitude = if rest.is_empty() {
        1
    } else if rest.bytes().all(|byte| byte.is_ascii_digit()) {
        rest.parse::<i64>()
            .map_err(|error| ExitFailure::new(1, format!("invalid window index: {error}")))?
    } else {
        return Ok(None);
    };
    Ok(Some(if sign == b'-' { -magnitude } else { magnitude }))
}

fn apply_window_index_offset(index: u32, offset: i64) -> Result<u32, ExitFailure> {
    let next = i64::from(index) + offset;
    if next < 0 || next > i64::from(u32::MAX) {
        return Err(ExitFailure::new(1, format!("can't find window: {next}")));
    }
    Ok(next as u32)
}

fn parse_bare_window_index(value: &str) -> Result<Option<u32>, ExitFailure> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|error| ExitFailure::new(1, format!("invalid window index: {error}")))
}

fn resolve_session_only_destination(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    if let Some(session) = target.raw().strip_suffix(':') {
        let session_target = crate::cli_args::parse_target_spec(session)
            .map_err(|error| ExitFailure::new(1, error))?;
        return resolve_session_target_spec(connection, &session_target, false);
    }
    resolve_session_target_spec(connection, target, false)
}

pub(super) fn first_available_window_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, ExitFailure> {
    let response = connection
        .list_windows(session_name.clone(), Some("#{window_index}".to_owned()))
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let used = String::from_utf8_lossy(output.stdout())
        .lines()
        .map(|line| {
            line.parse::<u32>().map_err(|error| {
                ExitFailure::new(1, format!("invalid list-windows index: {error}"))
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let base_index = session_base_index(connection, session_name)?;
    let mut index = base_index;
    loop {
        if !used.contains(&index) {
            return Ok(index);
        }
        let Some(next_index) = index.checked_add(1) else {
            return Err(ExitFailure::new(1, "window index space exhausted"));
        };
        index = next_index;
    }
}

fn session_base_index(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
) -> Result<u32, ExitFailure> {
    let response = connection
        .show_options(
            OptionScopeSelector::Session(session_name.clone()),
            Some("base-index".to_owned()),
            true,
            true,
            false,
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "show-options")?;
    let value = String::from_utf8_lossy(output.stdout());
    value
        .trim()
        .parse::<u32>()
        .map_err(|error| ExitFailure::new(1, format!("invalid base-index value: {error}")))
}

fn find_window_by_name(
    connection: &mut Connection,
    session_name: &rmux_proto::SessionName,
    name: &str,
) -> Result<Option<rmux_proto::WindowTarget>, ExitFailure> {
    let response = connection
        .list_windows(
            session_name.clone(),
            Some(format!(
                "#{{window_index}}{LIST_WINDOWS_FILTER_SEPARATOR}#{{window_name}}"
            )),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-windows")?;
    let mut matched = None;
    for line in String::from_utf8_lossy(output.stdout()).lines() {
        let Some((index, window_name)) = line.split_once(LIST_WINDOWS_FILTER_SEPARATOR) else {
            continue;
        };
        if window_name == name {
            let window_index = index
                .parse::<u32>()
                .map_err(|_| ExitFailure::new(1, format!("invalid window index: {index}")))?;
            let target = rmux_proto::WindowTarget::with_window(session_name.clone(), window_index);
            if matched.replace(target).is_some() {
                return Err(ExitFailure::new(
                    1,
                    format!("multiple windows named {name}"),
                ));
            }
        }
    }
    Ok(matched)
}

pub(super) fn signed_window_index_target(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.chars().all(|character| character.is_ascii_digit())
}
