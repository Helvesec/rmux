use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    DisplayPanesRequest, PaneTarget, Request, ResizePaneAdjustment, ResizePaneRelativeDirection,
    ResizePaneRequest, RmuxError, SelectCustomLayoutRequest, SelectLayoutRequest,
    SelectLayoutTarget, SelectOldLayoutRequest, SpreadLayoutRequest, TerminalSize,
};

use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{parse_percentage, parse_u64};
use super::{
    implicit_pane_target, implicit_session_name, implicit_window_target, parse_layout_name,
    parse_pane_target, parse_select_layout_target,
};

pub(super) fn parse_display_panes(
    mut args: CommandTokens,
    sessions: &SessionStore,
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
                let Some(cluster) = parse_compact_flag_cluster(&token, "bN", "dt") else {
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
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut spread = false;
    let mut old_layout = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-E" => {
                let _ = args.optional();
                spread = true;
            }
            "-o" => {
                let _ = args.optional();
                old_layout = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_select_layout_target(args.required("-t target")?)?);
            }
            _ => break,
        }
    }

    let target = target.unwrap_or(SelectLayoutTarget::Window(implicit_window_target(
        sessions,
        find_context,
        "select-layout",
    )?));
    if spread && old_layout {
        return Err(RmuxError::Server(
            "select-layout accepts only one of -E or -o".to_owned(),
        ));
    }
    if spread {
        args.no_extra("select-layout")?;
        return Ok(Request::SpreadLayout(SpreadLayoutRequest { target }));
    }
    if old_layout {
        args.no_extra("select-layout")?;
        return Ok(Request::SelectOldLayout(SelectOldLayoutRequest { target }));
    }

    let layout = args.required("select-layout layout")?;
    args.no_extra("select-layout")?;

    match parse_layout_name(&layout) {
        Ok(layout) => Ok(Request::SelectLayout(SelectLayoutRequest {
            target,
            layout,
        })),
        Err(_) => Ok(Request::SelectCustomLayout(SelectCustomLayoutRequest {
            target,
            layout,
        })),
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
    sessions: &SessionStore,
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
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut relative = None;
    let mut absolute_width = None;
    let mut absolute_height = None;
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
                absolute_height = Some(parse_resize_pane_size("-y", &args.required("-y value")?)?);
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
            _ => break,
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
    sessions: &SessionStore,
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

fn parse_resize_pane_relative(
    args: &mut CommandTokens,
    direction: ResizePaneRelativeDirection,
    flag: &str,
) -> Result<(ResizePaneRelativeDirection, u16, bool), RmuxError> {
    let (cells, explicit) = parse_resize_pane_delta(args, flag)?;
    if explicit && !args.is_empty() {
        return Err(RmuxError::Server(format!(
            "command resize-pane: too many arguments after {flag} adjustment"
        )));
    }
    Ok((direction, cells, explicit))
}

fn parse_resize_pane_delta(args: &mut CommandTokens, flag: &str) -> Result<(u16, bool), RmuxError> {
    match args.peek() {
        Some(next) if !next.starts_with('-') || next == "-" => Ok((
            parse_resize_pane_adjustment(&args.required(&format!("{flag} value"))?)?,
            true,
        )),
        _ => Ok((1, false)),
    }
}

fn parse_trailing_resize_pane_adjustment(
    args: &mut CommandTokens,
    relative: Option<(ResizePaneRelativeDirection, u16, bool)>,
) -> Result<Option<(ResizePaneRelativeDirection, u16, bool)>, RmuxError> {
    let Some((direction, cells, explicit)) = relative else {
        return Ok(None);
    };
    if explicit || args.is_empty() {
        return Ok(Some((direction, cells, explicit)));
    }
    let Some(next) = args.peek() else {
        return Ok(Some((direction, cells, explicit)));
    };
    if next.starts_with('-') && next != "-" {
        return Ok(Some((direction, cells, explicit)));
    }
    let value = args.required("resize-pane adjustment")?;
    let cells = parse_resize_pane_adjustment(&value)?;
    Ok(Some((direction, cells, true)))
}

fn parse_no_direction_trailing_resize_pane_adjustment(
    args: &mut CommandTokens,
) -> Result<(), RmuxError> {
    let Some(next) = args.peek() else {
        return Ok(());
    };
    if !integer_like_resize_pane_adjustment(next) {
        return Ok(());
    }
    let value = args.required("resize-pane adjustment")?;
    let _ = parse_resize_pane_adjustment(&value)?;
    Ok(())
}

fn parse_resize_pane_adjustment(value: &str) -> Result<u16, RmuxError> {
    let cells = match value.parse::<i128>() {
        Ok(value) => value,
        Err(_) if integer_like_resize_pane_adjustment(value) && value.starts_with('-') => {
            return Err(RmuxError::Server("adjustment too small".to_owned()));
        }
        Err(_) if integer_like_resize_pane_adjustment(value) => {
            return Err(RmuxError::Server("adjustment too large".to_owned()));
        }
        Err(error) => {
            return Err(RmuxError::Server(format!(
                "resize-pane adjustment invalid: {error}"
            )));
        }
    };
    if cells <= 0 {
        return Err(RmuxError::Server("adjustment too small".to_owned()));
    }
    if cells > i128::from(i32::MAX) {
        return Err(RmuxError::Server("adjustment too large".to_owned()));
    }
    Ok(u16::try_from(cells).unwrap_or(u16::MAX))
}

fn integer_like_resize_pane_adjustment(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn resize_pane_adjustment(
    columns: Option<u16>,
    rows: Option<u16>,
    relative: Option<(ResizePaneRelativeDirection, u16)>,
) -> Option<ResizePaneAdjustment> {
    match (columns, rows, relative) {
        (Some(columns), Some(rows), Some((relative, cells))) => {
            Some(ResizePaneAdjustment::Composite {
                columns: Some(columns),
                rows: Some(rows),
                relative: Some(relative),
                cells,
            })
        }
        (Some(columns), None, Some((relative, cells))) => Some(ResizePaneAdjustment::Composite {
            columns: Some(columns),
            rows: None,
            relative: Some(relative),
            cells,
        }),
        (None, Some(rows), Some((relative, cells))) => Some(ResizePaneAdjustment::Composite {
            columns: None,
            rows: Some(rows),
            relative: Some(relative),
            cells,
        }),
        (Some(columns), Some(rows), None) => {
            Some(ResizePaneAdjustment::AbsoluteSize { columns, rows })
        }
        (Some(columns), None, None) => Some(ResizePaneAdjustment::AbsoluteWidth { columns }),
        (None, Some(rows), None) => Some(ResizePaneAdjustment::AbsoluteHeight { rows }),
        (None, None, Some((relative, cells))) => Some(relative.to_adjustment(cells)),
        (None, None, None) => None,
    }
}
