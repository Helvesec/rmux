use rmux_client::Connection;
use rmux_proto::request::ListSessionsRequest;
use rmux_proto::{ErrorResponse, ResolveTargetType, Response, RmuxError};

use crate::cli_args::{parse_target_spec, TargetSpec};
use crate::cli_response::expect_command_output;

use super::{unexpected_response, ExitFailure};

pub(super) fn resolve_current_session_target(
    connection: &mut Connection,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    match connection
        .resolve_target(None, ResolveTargetType::Session, false, false)
        .map_err(ExitFailure::from_client)?
    {
        Response::ResolveTarget(response) => match response.target {
            rmux_proto::Target::Session(session_name) => Ok(session_name),
            other => Err(ExitFailure::new(
                1,
                format!(
                    "resolve-target produced {} where a session target was required",
                    response_name_for_target(&other)
                ),
            )),
        },
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(1, error.to_string())),
        other => Err(unexpected_response("resolve-target", &other)),
    }
}

pub(super) fn list_session_names(
    connection: &mut Connection,
) -> Result<Vec<rmux_proto::SessionName>, ExitFailure> {
    let response = connection
        .list_sessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        })
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "list-sessions")?;
    String::from_utf8_lossy(output.stdout())
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            rmux_proto::SessionName::new(line).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!("invalid session name from server '{line}': {error}"),
                )
            })
        })
        .collect()
}

pub(super) fn resolve_session_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
    prefer_unattached: bool,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    match resolve_target_spec(
        connection,
        target,
        ResolveTargetType::Session,
        false,
        prefer_unattached,
    )? {
        rmux_proto::Target::Session(session_name) => Ok(session_name),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a session target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

pub(super) fn resolve_window_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
    window_index: bool,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    if let Some(rmux_proto::Target::Window(target)) = target.exact() {
        return Ok(target.clone());
    }

    match resolve_target_spec(
        connection,
        target,
        ResolveTargetType::Window,
        window_index,
        false,
    )? {
        rmux_proto::Target::Window(target) => Ok(target),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a window target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

pub(super) fn resolve_existing_window_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    match resolve_target_spec(connection, target, ResolveTargetType::Window, false, false)? {
        rmux_proto::Target::Window(target) => Ok(target),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a window target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

pub(super) fn resolve_window_target_or_current(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    match target {
        Some(target) => resolve_window_target_spec(connection, target, false),
        None => {
            let pane = resolve_current_pane_target(connection, command_name)?;
            Ok(rmux_proto::WindowTarget::with_window(
                pane.session_name().clone(),
                pane.window_index(),
            ))
        }
    }
}

pub(super) fn resolve_window_index_target_or_current_session(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    if let Some(target) = target {
        return resolve_window_target_spec(connection, target, true);
    }

    let session_name = resolve_session_target_or_current(connection, None, command_name)?;
    let implicit = parse_target_spec(&format!("{session_name}:"))
        .map_err(|error| ExitFailure::new(1, error))?;
    resolve_window_target_spec(connection, &implicit, true)
}

pub(super) fn resolve_pane_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    match resolve_target_spec(connection, target, ResolveTargetType::Pane, false, false)? {
        rmux_proto::Target::Pane(target) => Ok(target),
        other => Err(ExitFailure::new(
            1,
            format!(
                "resolve-target produced {} where a pane target was required",
                response_name_for_target(&other)
            ),
        )),
    }
}

pub(super) fn resolve_existing_window_target_or_current(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::WindowTarget, ExitFailure> {
    match target {
        Some(target) => resolve_existing_window_target_spec(connection, target),
        None => {
            let pane = resolve_current_pane_target(connection, command_name)?;
            Ok(rmux_proto::WindowTarget::with_window(
                pane.session_name().clone(),
                pane.window_index(),
            ))
        }
    }
}

pub(super) fn resolve_pane_target_or_current(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    match target {
        Some(target) => resolve_pane_target_spec(connection, target),
        None => resolve_current_pane_target(connection, command_name),
    }
}

pub(super) fn resolve_split_window_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::SplitWindowTarget, ExitFailure> {
    Ok(rmux_proto::SplitWindowTarget::Pane(
        resolve_pane_target_spec(connection, target)?,
    ))
}

pub(super) fn resolve_select_layout_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
) -> Result<rmux_proto::SelectLayoutTarget, ExitFailure> {
    Ok(rmux_proto::SelectLayoutTarget::Window(
        resolve_window_target_spec(connection, target, false)?,
    ))
}

pub(super) fn resolve_target_spec(
    connection: &mut Connection,
    target: &TargetSpec,
    target_type: ResolveTargetType,
    window_index: bool,
    prefer_unattached: bool,
) -> Result<rmux_proto::Target, ExitFailure> {
    let response = connection
        .resolve_target(
            Some(target.raw().to_owned()),
            target_type,
            window_index,
            prefer_unattached,
        )
        .map_err(ExitFailure::from_client)?;
    match response {
        Response::ResolveTarget(response) => Ok(response.target),
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(
            1,
            target_resolution_error_message(&error, target_type, target.raw()),
        )),
        other => Err(unexpected_response("resolve-target", &other)),
    }
}

fn target_resolution_error_message(
    error: &RmuxError,
    target_type: ResolveTargetType,
    raw_target: &str,
) -> String {
    match error {
        RmuxError::InvalidTarget { reason, .. } if reason.starts_with("can't find ") => {
            reason.clone()
        }
        RmuxError::InvalidTarget { reason, .. }
            if target_type == ResolveTargetType::Pane
                && reason == "pane index does not exist in session" =>
        {
            format!("can't find pane: {}", pane_target_lookup_token(raw_target))
        }
        RmuxError::Server(message)
            if target_type == ResolveTargetType::Pane && message == "no current target" =>
        {
            format!("can't find pane: {}", pane_target_lookup_token(raw_target))
        }
        RmuxError::InvalidTarget { reason, .. }
            if target_type == ResolveTargetType::Window
                && reason == "window index does not exist in session" =>
        {
            format!(
                "can't find window: {}",
                window_target_lookup_token(raw_target)
            )
        }
        _ => error.to_string(),
    }
}

fn pane_target_lookup_token(raw_target: &str) -> &str {
    if raw_target.starts_with('%') {
        return raw_target;
    }
    raw_target
        .rsplit_once('.')
        .map_or(raw_target, |(_, pane)| pane)
}

fn window_target_lookup_token(raw_target: &str) -> &str {
    if raw_target.starts_with('@') {
        return raw_target;
    }
    raw_target
        .rsplit_once(':')
        .map_or(raw_target, |(_, window)| window)
}

pub(super) fn display_panes_client_target_error(raw_target: &str) -> ExitFailure {
    ExitFailure::new(
        1,
        format!(
            "can't find client: {}",
            raw_target.strip_suffix(':').unwrap_or(raw_target)
        ),
    )
}

pub(super) fn response_name_for_target(target: &rmux_proto::Target) -> &'static str {
    match target {
        rmux_proto::Target::Session(_) => "session target",
        rmux_proto::Target::Window(_) => "window target",
        rmux_proto::Target::Pane(_) => "pane target",
    }
}

pub(super) fn resolve_session_listing_target(
    connection: &mut Connection,
    target: Option<TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    if let Some(target) = target {
        return resolve_session_target_spec(connection, &target, false);
    }

    let _ = command_name;
    resolve_current_session_target(connection)
}

pub(super) fn resolve_session_target_or_current(
    connection: &mut Connection,
    target: Option<&TargetSpec>,
    command_name: &str,
) -> Result<rmux_proto::SessionName, ExitFailure> {
    match target {
        Some(target) => resolve_session_target_spec(connection, target, false),
        None => {
            let _ = command_name;
            resolve_current_session_target(connection)
        }
    }
}

pub(super) fn resolve_current_pane_target(
    connection: &mut Connection,
    command_name: &str,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    match connection
        .resolve_target(None, ResolveTargetType::Pane, false, false)
        .map_err(ExitFailure::from_client)?
    {
        Response::ResolveTarget(response) => match response.target {
            rmux_proto::Target::Pane(target) => Ok(target),
            other => Err(ExitFailure::new(
                1,
                format!(
                    "resolve-target produced {} where a pane target was required",
                    response_name_for_target(&other)
                ),
            )),
        },
        Response::Error(ErrorResponse { error }) => Err(ExitFailure::new(
            1,
            target_resolution_error_message(&error, ResolveTargetType::Pane, command_name),
        )),
        other => Err(unexpected_response("resolve-target", &other)),
    }
}
