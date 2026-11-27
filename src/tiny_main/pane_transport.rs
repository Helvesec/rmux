use std::io::IsTerminal;

use super::*;

pub(super) fn run_display_message(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyDisplayMessage,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let target = request.target;
    let has_explicit_target = target.is_some();
    let response = connection
        .display_message(target, true, request.message)
        .map_err(|error| error.to_string())?;
    if has_explicit_target && response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "display-message")
}

pub(super) fn run_send_keys(
    socket_path: &Path,
    request: TinySendKeys,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .resolve_target(
            Some(request.raw_target),
            ResolveTargetType::Pane,
            false,
            false,
        )
        .map_err(|error| error.to_string())?;
    let target = send_keys_resolved_target(response)?;
    let response = connection
        .send_keys(target, request.keys)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "send-keys")
}

pub(super) fn send_keys_resolved_target(
    response: Response,
) -> Result<rmux_proto::PaneTarget, String> {
    match response {
        Response::ResolveTarget(response) => match response.target {
            Target::Pane(target) => Ok(target),
            _ => Err(
                "protocol error: resolve-target returned a non-pane target for send-keys"
                    .to_owned(),
            ),
        },
        Response::Error(error) => Err(tmux_cli_error_message("send-keys", &error.error)),
        _ => Err("protocol error: unexpected response while resolving send-keys target".to_owned()),
    }
}

pub(super) fn run_new_window(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyNewWindow,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let start_directory = request.start_directory.or_else(|| env::current_dir().ok());
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .new_window_at_with_environment(
            request.target,
            None,
            request.name,
            request.detached,
            None,
            start_directory,
            request.command,
            false,
        )
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "new-window")
}

pub(super) fn run_kill_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyKillSession,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .kill_session(KillSessionRequest {
            target: request.target,
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        })
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "kill-session")
}

pub(super) fn run_show_options(
    socket_path: &Path,
    request: TinyShowOptions,
    command_name: &'static str,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .show_options(request.scope, None, false, false, false)
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command_name)
}

pub(super) fn run_rename_window(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyRenameWindow,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .rename_window(request.target, request.name.replace('\\', r"\\"))
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "rename-window")
}

pub(super) fn run_select_window(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinySelectWindow,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .select_window(request.target)
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "select-window")
}

pub(super) fn run_kill_pane(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyKillPane,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .kill_pane(request.target)
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "kill-pane")
}

pub(super) fn run_join_pane(
    original_args: &[OsString],
    socket_path: &Path,
    request: TinyJoinPane,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .join_pane(JoinPaneRequest {
            source: request.source,
            target: request.target,
            direction: SplitDirection::Vertical,
            detached: request.detached,
            before: false,
            full_size: false,
            size: None,
        })
        .map_err(|error| error.to_string())?;
    if response_needs_session_resolution_retry(&response) {
        return exec_full_helper(original_args);
    }
    write_response_output_or_error(response, "join-pane")
}

pub(super) fn run_set_option(
    socket_path: &Path,
    request: TinySetOption,
    command_name: &'static str,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = connection
        .set_option_by_name(
            request.scope,
            request.option,
            Some(request.value),
            SetOptionMode::Replace,
            false,
            false,
            false,
        )
        .map_err(|error| error.to_string())?;
    write_response_output_or_error(response, command_name)
}

pub(super) fn run_source_file(
    socket_path: &Path,
    request: TinySourceFile,
    command_connection: &mut TinyCommandConnection,
) -> Result<i32, String> {
    let mut connection = command_connection.connect(socket_path)?;
    let response = if !io::stdin().is_terminal()
        && connection
            .supports_capability(rmux_proto::CAPABILITY_SOURCE_FILE_INPUT_PULL_V1)
            .map_err(|error| error.to_string())?
    {
        let mut arguments = vec!["source-file".to_owned(), "--".to_owned()];
        arguments.extend(request.paths);
        let offer = rmux_proto::SourceFileInputV1Request::on_demand(
            arguments,
            std::env::current_dir().ok(),
        );
        crate::source_stdin::execute(&mut connection, socket_path, offer)
    } else {
        connection.source_file(request.paths, false, false, false, false, None, None)
    }
    .map_err(|error| error.to_string())?;
    write_source_file_response(response)
}

pub(super) fn run_new_session(
    original_args: &[OsString],
    socket_path: &Path,
    request: NewSessionExtRequest,
) -> Result<i32, String> {
    let mut connection = connect_with_validated_startup(socket_path)?;

    let response = connection.new_session_extended(request);
    if response_needs_full_helper(&response, RetryPolicy::DecodeOnly) {
        return exec_full_helper(original_args);
    }
    let response = response.map_err(|error| error.to_string())?;
    write_response_output_or_error(response, "new-session")
}
