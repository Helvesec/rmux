use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{
    CapturePaneRequest, CapturePaneTargetActionRequest, PaneTarget, Response, Target,
};

use crate::cli_args::{CapturePaneArgs, TargetSpec};
use crate::cli_response::expect_command_output;

use super::{
    capture_target_action_needs_legacy_retry, cli_target_actions_enabled,
    resolve_pane_target_or_current, ExitFailure,
};

#[derive(Clone)]
pub(super) struct PendingCapturePaneRequest {
    target: Option<TargetSpec>,
    start: CaptureBoundArg,
    end: CaptureBoundArg,
    print: bool,
    buffer_name: Option<String>,
    alternate: bool,
    escape_ansi: bool,
    escape_sequences: bool,
    include_format: bool,
    hyperlinks: bool,
    line_numbers: bool,
    join_wrapped: bool,
    use_mode_screen: bool,
    preserve_trailing_spaces: bool,
    do_not_trim_spaces: bool,
    pending_input: bool,
    quiet: bool,
}

/// A `-S` or `-E` value as typed: tmux reads it only after expanding it as a
/// format against the target, so it stays text until the server is reachable.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CaptureBoundArg {
    Default,
    Absolute,
    Value(String),
}

impl CaptureBoundArg {
    fn from_flag(value: Option<String>) -> Self {
        match value {
            None => Self::Default,
            Some(value) if value == "-" => Self::Absolute,
            Some(value) => Self::Value(value),
        }
    }
}

/// The bounds a capture request carries once read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CaptureBounds {
    start: Option<i64>,
    end: Option<i64>,
    start_is_absolute: bool,
    end_is_absolute: bool,
}

pub(super) fn capture_pane_request(
    args: CapturePaneArgs,
) -> Result<PendingCapturePaneRequest, ExitFailure> {
    Ok(PendingCapturePaneRequest {
        target: args.target,
        start: CaptureBoundArg::from_flag(args.start),
        end: CaptureBoundArg::from_flag(args.end),
        print: args.print,
        buffer_name: args.buffer_name,
        alternate: args.alternate,
        escape_ansi: args.escape_ansi,
        escape_sequences: args.escape_sequences,
        include_format: args.include_format,
        hyperlinks: args.hyperlinks,
        line_numbers: args.line_numbers,
        join_wrapped: args.join_wrapped,
        use_mode_screen: args.use_mode_screen,
        preserve_trailing_spaces: args.preserve_trailing_spaces,
        do_not_trim_spaces: args.do_not_trim_spaces,
        pending_input: args.pending_input,
        quiet: args.quiet,
    })
}

fn build_capture_pane_request(
    connection: &mut Connection,
    request: PendingCapturePaneRequest,
    bounds: CaptureBounds,
) -> Result<CapturePaneRequest, ExitFailure> {
    Ok(CapturePaneRequest {
        target: resolve_pane_target_or_current(
            connection,
            request.target.as_ref(),
            "capture-pane",
        )?,
        start: bounds.start,
        end: bounds.end,
        print: request.print,
        buffer_name: request.buffer_name,
        alternate: request.alternate,
        escape_ansi: request.escape_ansi,
        escape_sequences: request.escape_sequences,
        include_format: request.include_format,
        hyperlinks: request.hyperlinks,
        line_numbers: request.line_numbers,
        join_wrapped: request.join_wrapped,
        use_mode_screen: request.use_mode_screen,
        preserve_trailing_spaces: request.preserve_trailing_spaces,
        do_not_trim_spaces: request.do_not_trim_spaces,
        pending_input: request.pending_input,
        quiet: request.quiet,
        start_is_absolute: bounds.start_is_absolute,
        end_is_absolute: bounds.end_is_absolute,
    })
}

fn build_capture_pane_target_action_request(
    request: PendingCapturePaneRequest,
    bounds: CaptureBounds,
) -> CapturePaneTargetActionRequest {
    CapturePaneTargetActionRequest {
        target: request
            .target
            .as_ref()
            .map(|target| target.raw().to_owned()),
        start: bounds.start,
        end: bounds.end,
        print: request.print,
        buffer_name: request.buffer_name,
        alternate: request.alternate,
        escape_ansi: request.escape_ansi,
        escape_sequences: request.escape_sequences,
        include_format: request.include_format,
        hyperlinks: request.hyperlinks,
        line_numbers: request.line_numbers,
        join_wrapped: request.join_wrapped,
        use_mode_screen: request.use_mode_screen,
        preserve_trailing_spaces: request.preserve_trailing_spaces,
        do_not_trim_spaces: request.do_not_trim_spaces,
        pending_input: request.pending_input,
        quiet: request.quiet,
        start_is_absolute: bounds.start_is_absolute,
        end_is_absolute: bounds.end_is_absolute,
    }
}

pub(super) fn send_capture_pane_request(
    connection: &mut Connection,
    socket_path: &Path,
    request: PendingCapturePaneRequest,
) -> Result<Response, ExitFailure> {
    let bounds = read_capture_bounds(connection, &request)?;
    if !cli_target_actions_enabled() {
        let request = build_capture_pane_request(connection, request, bounds)?;
        return connection
            .capture_pane(request)
            .map_err(ExitFailure::from_client);
    }

    let legacy_request = request.clone();
    let response = connection
        .capture_pane_target_action(build_capture_pane_target_action_request(request, bounds));
    if !capture_target_action_needs_legacy_retry(&response) {
        return response.map_err(ExitFailure::from_client);
    }

    let mut legacy_connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let request = build_capture_pane_request(&mut legacy_connection, legacy_request, bounds)?;
    legacy_connection
        .capture_pane(request)
        .map_err(ExitFailure::from_client)
}

/// Reads both bounds once, before either transport is chosen, so the legacy
/// retry captures exactly the range the first attempt asked for.
fn read_capture_bounds(
    connection: &mut Connection,
    request: &PendingCapturePaneRequest,
) -> Result<CaptureBounds, ExitFailure> {
    let mut format_target = None;
    let (start, start_is_absolute) =
        read_capture_bound(connection, request, &request.start, &mut format_target)?;
    let (end, end_is_absolute) =
        read_capture_bound(connection, request, &request.end, &mut format_target)?;
    Ok(CaptureBounds {
        start,
        end,
        start_is_absolute,
        end_is_absolute,
    })
}

/// tmux never fails `capture-pane` on a bound: it expands the value as a
/// format against the target (`args_strtonum_and_expand`), and an unreadable
/// or out-of-range result selects the default bound, which `None` means on the
/// wire. Only a value that can name a format costs the extra round trip, and
/// the target is resolved for it once.
fn read_capture_bound(
    connection: &mut Connection,
    request: &PendingCapturePaneRequest,
    bound: &CaptureBoundArg,
    format_target: &mut Option<PaneTarget>,
) -> Result<(Option<i64>, bool), ExitFailure> {
    let value = match bound {
        CaptureBoundArg::Default => return Ok((None, false)),
        CaptureBoundArg::Absolute => return Ok((None, true)),
        CaptureBoundArg::Value(value) => value,
    };
    if !value.contains('#') {
        return Ok((rmux_core::parse_capture_bound(value), false));
    }
    let target = match format_target {
        Some(target) => target.clone(),
        None => {
            let target = resolve_pane_target_or_current(
                connection,
                request.target.as_ref(),
                "capture-pane",
            )?;
            format_target.insert(target).clone()
        }
    };
    let expanded = expand_bound_format(connection, target, value)?;
    Ok((rmux_core::parse_capture_bound(&expanded), false))
}

/// Expands `value` against `target` the way `format_single_from_target` does.
///
/// display-message also expands strftime tokens, which tmux does not do for a
/// bound, so every `%` is doubled first to reach the format layer unchanged
/// (`#{e|%:7,3}` stays a modulo, `%H` stays unreadable).
fn expand_bound_format(
    connection: &mut Connection,
    target: PaneTarget,
    value: &str,
) -> Result<String, ExitFailure> {
    let response = connection
        .display_message(
            Some(Target::Pane(target)),
            true,
            Some(value.replace('%', "%%")),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "capture-pane")?;
    let expanded = String::from_utf8_lossy(output.stdout());
    Ok(expanded.strip_suffix('\n').unwrap_or(&expanded).to_owned())
}
