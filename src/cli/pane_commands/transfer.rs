use std::path::Path;

use rmux_proto::{BreakPaneRequest, JoinPaneRequest, MovePaneRequest, PaneSplitSize};

use super::super::{
    resolve_pane_target_or_current, resolve_pane_target_spec, resolve_window_target_spec,
    run_command_resolved, ExitFailure,
};
use crate::cli_args::{parse_target_spec, BreakPaneArgs, JoinPaneArgs, SwapPaneArgs, TargetSpec};

pub(in crate::cli) fn run_swap_pane(
    args: SwapPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    if args.uses_relative_target() {
        if args.source.is_some() {
            return Err(ExitFailure::new(1, "swap-pane -D/-U does not accept -s"));
        }

        if args.down {
            return run_command_resolved(socket_path, "swap-pane", move |connection| {
                let target =
                    resolve_pane_target_or_current(connection, args.target.as_ref(), "swap-pane")?;
                connection
                    .swap_pane_with_next(target, args.detached, args.preserve_zoom)
                    .map_err(ExitFailure::from_client)
            });
        }

        return run_command_resolved(socket_path, "swap-pane", move |connection| {
            let target =
                resolve_pane_target_or_current(connection, args.target.as_ref(), "swap-pane")?;
            connection
                .swap_pane_with_previous(target, args.detached, args.preserve_zoom)
                .map_err(ExitFailure::from_client)
        });
    }

    run_command_resolved(socket_path, "swap-pane", move |connection| {
        let source = resolve_pane_source_or_marked(connection, args.source.as_ref())?;
        let target = resolve_pane_target_or_current(connection, args.target.as_ref(), "swap-pane")?;
        connection
            .swap_pane(source, target, args.detached, args.preserve_zoom)
            .map_err(ExitFailure::from_client)
    })
}

pub(in crate::cli) fn run_join_pane(
    args: JoinPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let direction = args.direction();
    let size = parse_pane_split_size(args.size_spec().as_deref())?;
    run_command_resolved(socket_path, "join-pane", move |connection| {
        let source = resolve_pane_source_or_marked(connection, args.source.as_ref())?;
        let target = resolve_pane_target_or_current(connection, args.target.as_ref(), "join-pane")?;
        connection
            .join_pane(JoinPaneRequest {
                source,
                target,
                direction,
                detached: args.detached,
                before: args.before,
                full_size: args.full_size,
                size,
            })
            .map_err(ExitFailure::from_client)
    })
}

pub(in crate::cli) fn run_break_pane(
    args: BreakPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    run_command_resolved(socket_path, "break-pane", move |connection| {
        let source =
            resolve_pane_target_or_current(connection, args.source.as_ref(), "break-pane")?;
        let target = args
            .target
            .as_ref()
            .map(|target| resolve_window_target_spec(connection, target, true))
            .transpose()?;
        connection
            .break_pane(BreakPaneRequest {
                source,
                target,
                name: args.name,
                detached: args.detached,
                after: args.after,
                before: args.before,
                print_target: args.print_target,
                format: args.format,
            })
            .map_err(ExitFailure::from_client)
    })
}

pub(in crate::cli) fn run_move_pane(
    args: JoinPaneArgs,
    socket_path: &Path,
) -> Result<i32, ExitFailure> {
    let direction = args.direction();
    let size = parse_pane_split_size(args.size_spec().as_deref())?;
    run_command_resolved(socket_path, "move-pane", move |connection| {
        let source = resolve_pane_source_or_marked(connection, args.source.as_ref())?;
        let target = resolve_pane_target_or_current(connection, args.target.as_ref(), "move-pane")?;
        connection
            .move_pane(MovePaneRequest {
                source,
                target,
                direction,
                detached: args.detached,
                before: args.before,
                full_size: args.full_size,
                size,
            })
            .map_err(ExitFailure::from_client)
    })
}

fn resolve_pane_source_or_marked(
    connection: &mut rmux_client::Connection,
    source: Option<&TargetSpec>,
) -> Result<rmux_proto::PaneTarget, ExitFailure> {
    if let Some(source) = source {
        return resolve_pane_target_spec(connection, source);
    }

    let marked = parse_target_spec("{marked}").map_err(|error| ExitFailure::new(1, error))?;
    resolve_pane_target_spec(connection, &marked)
        .or_else(|_| resolve_pane_target_or_current(connection, None, "pane source"))
}

fn parse_pane_split_size(value: Option<&str>) -> Result<Option<PaneSplitSize>, ExitFailure> {
    let Some(value) = value else {
        return Ok(None);
    };

    if let Some(percentage) = value.strip_suffix('%') {
        let percentage = percentage.parse::<u8>().map_err(|error| {
            ExitFailure::new(
                1,
                format!("invalid pane size percentage '{value}': {error}"),
            )
        })?;
        if percentage > 100 {
            return Err(ExitFailure::new(
                1,
                format!("invalid pane size percentage '{value}': must be between 0 and 100"),
            ));
        }
        return Ok(Some(PaneSplitSize::Percentage(percentage)));
    }

    value
        .parse::<u32>()
        .map(PaneSplitSize::Absolute)
        .map(Some)
        .map_err(|error| ExitFailure::new(1, format!("invalid pane size '{value}': {error}")))
}
