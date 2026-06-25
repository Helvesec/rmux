use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{CapturePaneRequest, CapturePaneTargetActionRequest, Response};

use crate::cli_args::{CapturePaneArgs, TargetSpec};

use super::{
    capture_target_action_needs_legacy_retry, cli_target_actions_enabled,
    resolve_pane_target_or_current, ExitFailure,
};

#[derive(Clone)]
pub(super) struct PendingCapturePaneRequest {
    target: Option<TargetSpec>,
    start: Option<i64>,
    end: Option<i64>,
    print: bool,
    buffer_name: Option<String>,
    alternate: bool,
    escape_ansi: bool,
    escape_sequences: bool,
    join_wrapped: bool,
    use_mode_screen: bool,
    preserve_trailing_spaces: bool,
    do_not_trim_spaces: bool,
    pending_input: bool,
    quiet: bool,
    start_is_absolute: bool,
    end_is_absolute: bool,
}

pub(super) fn capture_pane_request(
    args: CapturePaneArgs,
) -> Result<PendingCapturePaneRequest, ExitFailure> {
    let (start, start_is_absolute) = parse_capture_bound(args.start.as_deref(), "-S")?;
    let (end, end_is_absolute) = parse_capture_bound(args.end.as_deref(), "-E")?;

    Ok(PendingCapturePaneRequest {
        target: args.target,
        start,
        end,
        print: args.print,
        buffer_name: args.buffer_name,
        alternate: args.alternate,
        escape_ansi: args.escape_ansi,
        escape_sequences: args.escape_sequences,
        join_wrapped: args.join_wrapped,
        use_mode_screen: false,
        preserve_trailing_spaces: args.preserve_trailing_spaces,
        do_not_trim_spaces: args.do_not_trim_spaces,
        pending_input: args.pending_input,
        quiet: args.quiet,
        start_is_absolute,
        end_is_absolute,
    })
}

pub(super) fn build_capture_pane_request(
    connection: &mut Connection,
    request: PendingCapturePaneRequest,
) -> Result<CapturePaneRequest, ExitFailure> {
    Ok(CapturePaneRequest {
        target: resolve_pane_target_or_current(
            connection,
            request.target.as_ref(),
            "capture-pane",
        )?,
        start: request.start,
        end: request.end,
        print: request.print,
        buffer_name: request.buffer_name,
        alternate: request.alternate,
        escape_ansi: request.escape_ansi,
        escape_sequences: request.escape_sequences,
        join_wrapped: request.join_wrapped,
        use_mode_screen: request.use_mode_screen,
        preserve_trailing_spaces: request.preserve_trailing_spaces,
        do_not_trim_spaces: request.do_not_trim_spaces,
        pending_input: request.pending_input,
        quiet: request.quiet,
        start_is_absolute: request.start_is_absolute,
        end_is_absolute: request.end_is_absolute,
    })
}

pub(super) fn build_capture_pane_target_action_request(
    request: PendingCapturePaneRequest,
) -> CapturePaneTargetActionRequest {
    CapturePaneTargetActionRequest {
        target: request
            .target
            .as_ref()
            .map(|target| target.raw().to_owned()),
        start: request.start,
        end: request.end,
        print: request.print,
        buffer_name: request.buffer_name,
        alternate: request.alternate,
        escape_ansi: request.escape_ansi,
        escape_sequences: request.escape_sequences,
        join_wrapped: request.join_wrapped,
        use_mode_screen: request.use_mode_screen,
        preserve_trailing_spaces: request.preserve_trailing_spaces,
        do_not_trim_spaces: request.do_not_trim_spaces,
        pending_input: request.pending_input,
        quiet: request.quiet,
        start_is_absolute: request.start_is_absolute,
        end_is_absolute: request.end_is_absolute,
    }
}

pub(super) fn send_capture_pane_request(
    connection: &mut Connection,
    socket_path: &Path,
    request: PendingCapturePaneRequest,
) -> Result<Response, ExitFailure> {
    if !cli_target_actions_enabled() {
        let request = build_capture_pane_request(connection, request)?;
        return connection
            .capture_pane(request)
            .map_err(ExitFailure::from_client);
    }

    let legacy_request = request.clone();
    let response =
        connection.capture_pane_target_action(build_capture_pane_target_action_request(request));
    if !capture_target_action_needs_legacy_retry(&response) {
        return response.map_err(ExitFailure::from_client);
    }

    let mut legacy_connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let request = build_capture_pane_request(&mut legacy_connection, legacy_request)?;
    legacy_connection
        .capture_pane(request)
        .map_err(ExitFailure::from_client)
}

fn parse_capture_bound(
    value: Option<&str>,
    flag: &str,
) -> Result<(Option<i64>, bool), ExitFailure> {
    match value {
        None => Ok((None, false)),
        Some("-") => Ok((None, true)),
        Some(value) => value
            .parse::<i64>()
            .map(|value| (Some(value), false))
            .map_err(|_| {
                ExitFailure::new(1, format!("command capture-pane: {flag} expects a number"))
            }),
    }
}
