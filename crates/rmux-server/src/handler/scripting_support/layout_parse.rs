use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{
    DisplayPanesRequest, NextLayoutRequest, PaneTarget, PreviousLayoutRequest, Request,
    ResizePaneAdjustment, ResizePaneRelativeDirection, ResizePaneRequest, RmuxError,
    SelectCustomLayoutRequest, SelectLayoutRequest, SelectLayoutTarget, SelectOldLayoutRequest,
    SpreadLayoutRequest, TerminalSize, WindowTarget,
};

use crate::pane_terminals::session_not_found;

use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{parse_percentage, parse_u64, reject_unknown_option_before_positional};
use super::{
    implicit_pane_target, implicit_session_name, implicit_window_target, parse_layout_name,
    parse_pane_target, parse_select_layout_target,
};

mod resize_adjustment;
use resize_adjustment::{
    integer_like_resize_pane_adjustment, parse_no_direction_trailing_resize_pane_adjustment,
    parse_resize_pane_relative, parse_trailing_resize_pane_adjustment, resize_pane_adjustment,
};

#[derive(Debug)]
pub(super) enum ParsedSelectLayout {
    NoOp,
    Request(Request),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectLayoutMode {
    Next,
    Previous,
    Spread,
    Old,
}

pub(super) fn parse_display_panes(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut duration_ms = None;
    let mut non_blocking = false;
    let mut no_command = false;
    let mut target_client = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-b" => {
                let _ = args.optional();
                non_blocking = true;
            }
            "-d" => {
                let _ = args.optional();
                duration_ms = Some(parse_u64(
                    "display-panes",
                    "-d",
                    &args.required("-d duration")?,
                )?);
            }
            "-N" => {
                let _ = args.optional();
                no_command = true;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            _ => {
                let Some(cluster) =
                    parse_compact_flag_cluster("display-panes", &token, "bN", "dt")?
                else {
                    reject_unknown_option_before_positional("display-panes", &token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('b') => non_blocking = true,
                        CompactFlag::Bare('N') => no_command = true,
                        compact_flag @ CompactFlag::Value { flag: 'd', .. } => {
                            let value = compact_flag.value_or_next(&mut args, "-d duration")?;
                            duration_ms = Some(parse_u64("display-panes", "-d", &value)?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-t target-client")?);
                        }
                        _ => unreachable!("compact display-panes flags are prevalidated"),
                    }
                }
            }
        }
    }

    let template = (!args.is_empty()).then(|| args.remaining_joined());

    Ok(Request::DisplayPanes(Box::new(DisplayPanesRequest {
        target: implicit_session_name(sessions, find_context, "display-panes")?,
        duration_ms,
        non_blocking,
        no_command,
        template,
        target_client,
    })))
}

pub(super) fn parse_select_layout(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedSelectLayout, RmuxError> {
    let mut target = None;
    let mut spread = false;
    let mut next_layout = false;
    let mut old_layout = false;
    let mut previous_layout = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-E" => {
                let _ = args.optional();
                spread = true;
            }
            "-n" => {
                let _ = args.optional();
                next_layout = true;
            }
            "-o" => {
                let _ = args.optional();
                old_layout = true;
            }
            "-p" => {
                let _ = args.optional();
                previous_layout = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_select_layout_target(args.required("-t target")?)?);
            }
            _ => {
                let Some(cluster) =
                    parse_compact_flag_cluster("select-layout", &token, "Enop", "t")?
                else {
                    reject_unknown_option_before_positional("select-layout", &token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('E') => spread = true,
                        CompactFlag::Bare('n') => next_layout = true,
                        CompactFlag::Bare('o') => old_layout = true,
                        CompactFlag::Bare('p') => previous_layout = true,
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_select_layout_target(
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        _ => unreachable!("compact select-layout flags are prevalidated"),
                    }
                }
            }
        }
    }

    let target = target.unwrap_or(SelectLayoutTarget::Window(implicit_window_target(
        sessions,
        find_context,
        "select-layout",
    )?));
    let mode = if next_layout {
        Some(SelectLayoutMode::Next)
    } else if previous_layout {
        Some(SelectLayoutMode::Previous)
    } else if spread {
        Some(SelectLayoutMode::Spread)
    } else if old_layout {
        Some(SelectLayoutMode::Old)
    } else {
        None
    };
    let layout = args.optional();
    args.no_extra("select-layout")?;

    match mode {
        Some(SelectLayoutMode::Next) => {
            return Ok(ParsedSelectLayout::Request(Request::NextLayout(
                NextLayoutRequest {
                    target: select_layout_window_target(&target, sessions)?,
                },
            )));
        }
        Some(SelectLayoutMode::Previous) => {
            return Ok(ParsedSelectLayout::Request(Request::PreviousLayout(
                PreviousLayoutRequest {
                    target: select_layout_window_target(&target, sessions)?,
                },
            )));
        }
        Some(SelectLayoutMode::Spread) => {
            return Ok(ParsedSelectLayout::Request(Request::SpreadLayout(
                SpreadLayoutRequest { target },
            )));
        }
        Some(SelectLayoutMode::Old) => {
            return Ok(ParsedSelectLayout::Request(match layout {
                Some(layout) => {
                    Request::SelectCustomLayout(SelectCustomLayoutRequest { target, layout })
                }
                None => Request::SelectOldLayout(SelectOldLayoutRequest { target }),
            }));
        }
        None => {}
    }

    let Some(layout) = layout else {
        return Ok(ParsedSelectLayout::NoOp);
    };

    match parse_layout_name(&layout) {
        Ok(layout) => Ok(ParsedSelectLayout::Request(Request::SelectLayout(
            SelectLayoutRequest { target, layout },
        ))),
        Err(_) => Ok(ParsedSelectLayout::Request(Request::SelectCustomLayout(
            SelectCustomLayoutRequest { target, layout },
        ))),
    }
}

fn select_layout_window_target(
    target: &SelectLayoutTarget,
    sessions: &ManagedSessionStore,
) -> Result<WindowTarget, RmuxError> {
    match target {
        SelectLayoutTarget::Window(target) => Ok(target.clone()),
        SelectLayoutTarget::Session(session_name) => {
            let session = sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            Ok(WindowTarget::with_window(
                session_name.clone(),
                session.active_window_index(),
            ))
        }
    }
}

#[derive(Clone, Copy)]
enum ResizePaneSize {
    Cells(u16),
    Percent(u8),
}

#[derive(Clone, Copy)]
enum ResizeAxis {
    Width,
    Height,
}

impl ResizePaneSize {
    fn resolve(self, window_size: Option<TerminalSize>, axis: ResizeAxis) -> Option<u16> {
        match self {
            Self::Cells(cells) => Some(cells),
            Self::Percent(percent) => {
                let total = match axis {
                    ResizeAxis::Width => window_size?.cols,
                    ResizeAxis::Height => window_size?.rows,
                };
                let cells = u32::from(total) * u32::from(percent) / 100;
                Some(u16::try_from(cells.max(1)).unwrap_or(u16::MAX))
            }
        }
    }
}

fn parse_resize_pane_size(flag: &str, value: &str) -> Result<ResizePaneSize, RmuxError> {
    if let Some(percent) = value.strip_suffix('%') {
        return parse_percentage("resize-pane", flag, percent).map(ResizePaneSize::Percent);
    }
    parse_resize_pane_cells(flag, value).map(ResizePaneSize::Cells)
}

fn parse_resize_pane_cells(flag: &str, value: &str) -> Result<u16, RmuxError> {
    let cells = value.parse::<i64>().map_err(|error| {
        // A malformed height is deferred until after a valid width commits.
        // Keep width, range and percentage diagnostics in their existing paths.
        if flag == "-y"
            && matches!(
                error.kind(),
                std::num::IntErrorKind::Empty | std::num::IntErrorKind::InvalidDigit
            )
        {
            return RmuxError::Server("height invalid".to_owned());
        }
        RmuxError::Server(format!(
            "resize-pane {flag} expects an integer cell count: {error}"
        ))
    })?;
    if cells < 0 {
        return Err(RmuxError::Server(format!(
            "resize-pane {flag} expects a non-negative cell count"
        )));
    }
    if cells > i64::from(i32::MAX) {
        return Err(RmuxError::Server(format!(
            "resize-pane {flag} cell count is too large"
        )));
    }
    Ok(u16::try_from(cells).unwrap_or(u16::MAX))
}

fn resize_pane_uses_percent(width: Option<ResizePaneSize>, height: Option<ResizePaneSize>) -> bool {
    matches!(width, Some(ResizePaneSize::Percent(_)))
        || matches!(height, Some(ResizePaneSize::Percent(_)))
}

fn resize_pane_window_size(
    sessions: &ManagedSessionStore,
    target: &PaneTarget,
) -> Result<TerminalSize, RmuxError> {
    let session = sessions.session(target.session_name()).ok_or_else(|| {
        RmuxError::Server(format!(
            "resize-pane could not resolve dimensions for pane {target}"
        ))
    })?;
    let window = session.window_at(target.window_index()).ok_or_else(|| {
        RmuxError::Server(format!(
            "resize-pane could not resolve dimensions for pane {target}"
        ))
    })?;
    if window.pane(target.pane_index()).is_none() {
        return Err(RmuxError::Server(format!(
            "resize-pane could not resolve dimensions for pane {target}"
        )));
    }
    Ok(window.size())
}

pub(super) fn parse_resize_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut relative = None;
    let mut absolute_width = None;
    let mut absolute_height = None;
    let mut height_error = None;
    let mut trim_below = false;
    let mut zoom = false;
    let mut relative_seen = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "resize-pane",
                    args.required("-t target")?,
                )?);
            }
            "-x" => {
                let _ = args.optional();
                absolute_width = Some(parse_resize_pane_size("-x", &args.required("-x value")?)?);
            }
            "-y" => {
                let _ = args.optional();
                match parse_resize_pane_size("-y", &args.required("-y value")?) {
                    Ok(size) => {
                        absolute_height = Some(size);
                        height_error = None;
                    }
                    Err(error) => {
                        absolute_height = None;
                        height_error = Some(error);
                    }
                }
            }
            "-U" => {
                let _ = args.optional();
                if relative_seen {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one relative adjustment".to_owned(),
                    ));
                }
                relative_seen = true;
                relative = Some(parse_resize_pane_relative(
                    &mut args,
                    ResizePaneRelativeDirection::Up,
                    "-U",
                )?);
            }
            "-D" => {
                let _ = args.optional();
                if relative_seen {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one relative adjustment".to_owned(),
                    ));
                }
                relative_seen = true;
                relative = Some(parse_resize_pane_relative(
                    &mut args,
                    ResizePaneRelativeDirection::Down,
                    "-D",
                )?);
            }
            "-L" => {
                let _ = args.optional();
                if relative_seen {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one relative adjustment".to_owned(),
                    ));
                }
                relative_seen = true;
                relative = Some(parse_resize_pane_relative(
                    &mut args,
                    ResizePaneRelativeDirection::Left,
                    "-L",
                )?);
            }
            "-R" => {
                let _ = args.optional();
                if relative_seen {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one relative adjustment".to_owned(),
                    ));
                }
                relative_seen = true;
                relative = Some(parse_resize_pane_relative(
                    &mut args,
                    ResizePaneRelativeDirection::Right,
                    "-R",
                )?);
            }
            "-Z" => {
                let _ = args.optional();
                zoom = true;
            }
            "-T" => {
                let _ = args.optional();
                trim_below = true;
            }
            "-M" => {
                let _ = args.optional();
            }
            _ => {
                if token.starts_with('-') && !integer_like_resize_pane_adjustment(token) {
                    let _ = parse_compact_flag_cluster("resize-pane", token, "DLMRTUZ", "txy")?;
                }
                break;
            }
        }
    }
    relative = parse_trailing_resize_pane_adjustment(&mut args, relative)?;
    if relative.is_none() {
        parse_no_direction_trailing_resize_pane_adjustment(&mut args)?;
    }
    args.no_extra("resize-pane")?;
    let target = target.unwrap_or(implicit_pane_target(sessions, find_context, "resize-pane")?);
    let window_size = if resize_pane_uses_percent(absolute_width, absolute_height) {
        Some(resize_pane_window_size(sessions, &target)?)
    } else {
        None
    };
    let absolute_width =
        absolute_width.and_then(|size| size.resolve(window_size, ResizeAxis::Width));
    let absolute_height =
        absolute_height.and_then(|size| size.resolve(window_size, ResizeAxis::Height));
    if !trim_below && !zoom {
        if let Some(error) = height_error {
            // Structural flags and target resolution have already succeeded.
            // Height is a distinct execution phase, after width, as measured.
            return Ok(Request::ResizePaneAxes(Box::new(
                rmux_proto::ResizePaneAxesRequest {
                    target,
                    columns: absolute_width,
                    rows: Err(error),
                },
            )));
        }
    }
    let adjustment = if trim_below {
        Some(ResizePaneAdjustment::TrimBelow)
    } else if zoom {
        Some(ResizePaneAdjustment::Zoom)
    } else {
        resize_pane_adjustment(
            absolute_width,
            absolute_height,
            relative.map(|(direction, cells, _)| (direction, cells)),
        )
    };

    Ok(Request::ResizePane(ResizePaneRequest {
        target,
        adjustment: adjustment.unwrap_or(ResizePaneAdjustment::NoOp),
    }))
}

pub(super) fn parse_resize_pane_mouse_target(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Option<rmux_proto::PaneTarget>, RmuxError> {
    let mut target = None;
    let mut mouse_resize = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "resize-pane",
                    args.required("-t target")?,
                )?);
            }
            "-M" => {
                let _ = args.optional();
                mouse_resize = true;
            }
            "-D" | "-U" | "-L" | "-R" | "-Z" | "-T" | "-x" | "-y" => {
                return Ok(None);
            }
            _ => break,
        }
    }

    if !mouse_resize {
        return Ok(None);
    }
    args.no_extra("resize-pane")?;
    Ok(Some(target.unwrap_or(implicit_pane_target(
        sessions,
        find_context,
        "resize-pane",
    )?)))
}

#[cfg(test)]
#[path = "../../handler_scripting/layout_parse_tests.rs"]
mod tests;
