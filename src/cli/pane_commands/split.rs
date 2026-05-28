use std::path::Path;

use rmux_client::{connect, Connection};
use rmux_proto::{ErrorResponse, ResizePaneAdjustment, Response};

use super::super::format_print::print_target_format;
use super::super::{
    expect_command_output, expect_command_success, resolve_current_pane_target,
    resolve_split_window_target_spec, unexpected_response, ExitFailure,
};
use crate::cli_args::SplitWindowArgs;

const DEFAULT_SPLIT_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

#[derive(Debug, Clone)]
enum SplitAnchor {
    Exact(rmux_proto::PaneTarget),
    SessionCurrent(rmux_proto::SessionName),
}

#[derive(Debug, Clone, Copy)]
enum SplitSizeSpec {
    Absolute(u16),
    Percentage(u8),
}

pub(in crate::cli) fn run_split_window(
    args: SplitWindowArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let direction = args.direction();
    let print_target = args.print_target;
    let print_format = args
        .format
        .clone()
        .unwrap_or_else(|| DEFAULT_SPLIT_WINDOW_PRINT_FORMAT.to_owned());
    let mut connection = connect(socket_path)
        .map_err(|error| ExitFailure::from_client_connect(socket_path, error))?;
    let target = match args.target.as_ref() {
        Some(target) => resolve_split_window_target_spec(&mut connection, target)?,
        None => rmux_proto::SplitWindowTarget::Pane(resolve_current_pane_target(
            &mut connection,
            "split-window",
        )?),
    };
    let anchor = if args.detached {
        Some(split_anchor_for_target(&target))
    } else {
        None
    };
    let requested_size = requested_split_resize_adjustment(
        &mut connection,
        &target,
        direction,
        args.size.as_deref(),
    )?;
    let response = connection
        .split_window_with_start_directory(
            target.clone(),
            direction,
            (!args.environment.is_empty()).then_some(args.environment),
            args.start_directory,
            (!args.command.is_empty()).then_some(args.command),
        )
        .map_err(ExitFailure::from_client)?;
    let pane = match response {
        Response::SplitWindow(response) => response.pane,
        Response::Error(ErrorResponse { error }) => {
            return Err(ExitFailure::new(1, error.to_string()))
        }
        other => return Err(unexpected_response("split-window", &other)),
    };

    if let Some(adjustment) = requested_size {
        let response = connection
            .resize_pane(pane.clone(), adjustment)
            .map_err(ExitFailure::from_client)?;
        expect_command_success(response, "resize-pane")?;
    }

    if let Some(anchor) = anchor {
        let anchor = resolve_split_anchor(&mut connection, anchor)?;
        let response = connection
            .select_pane(anchor)
            .map_err(ExitFailure::from_client)?;
        expect_command_success(response, "select-pane")?;
    }

    if print_target {
        print_target_format(
            &mut connection,
            "split-window",
            rmux_proto::Target::Pane(pane),
            &print_format,
        )?;
    }

    Ok(0)
}

fn requested_split_resize_adjustment(
    connection: &mut Connection,
    target: &rmux_proto::SplitWindowTarget,
    direction: rmux_proto::SplitDirection,
    size: Option<&str>,
) -> Result<Option<ResizePaneAdjustment>, ExitFailure> {
    let Some(size) = size else {
        return Ok(None);
    };
    let parsed = parse_split_size_spec(size)?;
    let amount = match parsed {
        SplitSizeSpec::Absolute(value) => value,
        SplitSizeSpec::Percentage(percentage) => {
            let (window_cols, window_rows) = split_target_window_size(connection, target)?;
            let total = match direction {
                rmux_proto::SplitDirection::Vertical => window_cols,
                rmux_proto::SplitDirection::Horizontal => window_rows,
            };
            let scaled =
                ((u32::from(total) * u32::from(percentage)) / 100).clamp(1, u32::from(u16::MAX));
            scaled as u16
        }
    };

    Ok(Some(match direction {
        rmux_proto::SplitDirection::Vertical => {
            ResizePaneAdjustment::AbsoluteWidth { columns: amount }
        }
        rmux_proto::SplitDirection::Horizontal => {
            ResizePaneAdjustment::AbsoluteHeight { rows: amount }
        }
    }))
}

fn parse_split_size_spec(value: &str) -> Result<SplitSizeSpec, ExitFailure> {
    if let Some(percentage) = value.strip_suffix('%') {
        let percentage = percentage.parse::<u8>().map_err(|error| {
            ExitFailure::new(
                1,
                format!("invalid split size percentage '{value}': {error}"),
            )
        })?;
        if percentage == 0 || percentage > 100 {
            return Err(ExitFailure::new(
                1,
                format!("invalid split size percentage '{value}': must be 1..=100"),
            ));
        }
        return Ok(SplitSizeSpec::Percentage(percentage));
    }

    let absolute = value
        .parse::<u16>()
        .map_err(|error| ExitFailure::new(1, format!("invalid split size '{value}': {error}")))?;
    Ok(SplitSizeSpec::Absolute(absolute.max(1)))
}

fn split_target_window_size(
    connection: &mut Connection,
    target: &rmux_proto::SplitWindowTarget,
) -> Result<(u16, u16), ExitFailure> {
    let target = match target {
        rmux_proto::SplitWindowTarget::Session(session_name) => {
            rmux_proto::Target::Session(session_name.clone())
        }
        rmux_proto::SplitWindowTarget::Pane(target) => rmux_proto::Target::Pane(target.clone()),
    };
    let response = connection
        .display_message(
            Some(target),
            true,
            Some("#{window_width} #{window_height}".to_owned()),
        )
        .map_err(ExitFailure::from_client)?;
    let output = expect_command_output(&response, "display-message")?;
    let value = String::from_utf8_lossy(output.stdout()).trim().to_owned();
    let (cols, rows) = value
        .split_once(' ')
        .ok_or_else(|| ExitFailure::new(1, format!("invalid split size response: {value}")))?;
    let cols = cols.parse::<u16>().map_err(|error| {
        ExitFailure::new(1, format!("invalid split size width '{value}': {error}"))
    })?;
    let rows = rows.parse::<u16>().map_err(|error| {
        ExitFailure::new(1, format!("invalid split size height '{value}': {error}"))
    })?;
    Ok((cols, rows))
}

fn split_anchor_for_target(target: &rmux_proto::SplitWindowTarget) -> SplitAnchor {
    match target {
        rmux_proto::SplitWindowTarget::Pane(target) => SplitAnchor::Exact(target.clone()),
        rmux_proto::SplitWindowTarget::Session(session_name) => {
            SplitAnchor::SessionCurrent(session_name.clone())
        }
    }
}

fn resolve_split_anchor(
    connection: &mut Connection,
    anchor: SplitAnchor,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    match anchor {
        SplitAnchor::Exact(target) => Ok(target),
        SplitAnchor::SessionCurrent(session_name) => {
            let response = connection
                .display_message(
                    Some(rmux_proto::Target::Session(session_name.clone())),
                    true,
                    Some("#{window_index}.#{pane_index}".to_owned()),
                )
                .map_err(ExitFailure::from_client)?;
            let output = expect_command_output(&response, "display-message")?;
            let value = String::from_utf8_lossy(output.stdout()).trim().to_owned();
            let (window_index, pane_index) = value.split_once('.').ok_or_else(|| {
                ExitFailure::new(1, format!("invalid split anchor response: {value}"))
            })?;
            let window_index = window_index.parse::<u32>().map_err(|error| {
                ExitFailure::new(
                    1,
                    format!("invalid split anchor window index '{value}': {error}"),
                )
            })?;
            let pane_index = pane_index.parse::<u32>().map_err(|error| {
                ExitFailure::new(
                    1,
                    format!("invalid split anchor pane index '{value}': {error}"),
                )
            })?;
            Ok(rmux_proto::PaneTarget::with_window(
                session_name,
                window_index,
                pane_index,
            ))
        }
    }
}
