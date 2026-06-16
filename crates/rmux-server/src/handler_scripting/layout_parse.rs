use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{
    DisplayPanesRequest, Request, ResizePaneAdjustment, ResizePaneRequest, RmuxError,
    SelectCustomLayoutRequest, SelectLayoutRequest, SelectLayoutTarget, SelectOldLayoutRequest,
    SpreadLayoutRequest,
};

use super::tokens::CommandTokens;
use super::values::{parse_u16, parse_u64};
use super::{
    implicit_pane_target, implicit_session_name, implicit_window_target,
    is_unsupported_named_layout, parse_layout_name, parse_pane_target, parse_select_layout_target,
};

pub(super) fn parse_display_panes(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut duration_ms = None;
    let mut non_blocking = false;
    let mut no_command = false;

    while let Some(token) = args.peek() {
        match token {
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
                return Err(display_panes_client_target_not_found(
                    &args.required("-t target-client")?,
                ));
            }
            _ => break,
        }
    }

    let template = (!args.is_empty()).then(|| args.remaining_joined());

    Ok(Request::DisplayPanes(DisplayPanesRequest {
        target: implicit_session_name(sessions, find_context, "display-panes")?,
        duration_ms,
        non_blocking,
        no_command,
        template,
    }))
}

fn display_panes_client_target_not_found(raw_target: &str) -> RmuxError {
    RmuxError::Message(format!(
        "can't find client: {}",
        raw_target.strip_suffix(':').unwrap_or(raw_target)
    ))
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
        Ok(layout) if is_unsupported_named_layout(layout) => {
            Err(RmuxError::Server(format!("invalid layout: {layout}")))
        }
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

pub(super) fn parse_resize_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut adjustment = None;
    let mut absolute_width = None;
    let mut absolute_height = None;
    let mut trim_below = false;

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
                if adjustment.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                absolute_width = Some(parse_u16("resize-pane", "-x", &args.required("-x value")?)?);
            }
            "-y" => {
                let _ = args.optional();
                if adjustment.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                absolute_height =
                    Some(parse_u16("resize-pane", "-y", &args.required("-y value")?)?);
            }
            "-U" => {
                let _ = args.optional();
                if adjustment.is_some() || absolute_width.is_some() || absolute_height.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                adjustment = Some(ResizePaneAdjustment::Up {
                    cells: parse_resize_pane_delta(&mut args, "-U")?,
                });
            }
            "-D" => {
                let _ = args.optional();
                if adjustment.is_some() || absolute_width.is_some() || absolute_height.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                adjustment = Some(ResizePaneAdjustment::Down {
                    cells: parse_resize_pane_delta(&mut args, "-D")?,
                });
            }
            "-L" => {
                let _ = args.optional();
                if adjustment.is_some() || absolute_width.is_some() || absolute_height.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                adjustment = Some(ResizePaneAdjustment::Left {
                    cells: parse_resize_pane_delta(&mut args, "-L")?,
                });
            }
            "-R" => {
                let _ = args.optional();
                if adjustment.is_some() || absolute_width.is_some() || absolute_height.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                adjustment = Some(ResizePaneAdjustment::Right {
                    cells: parse_resize_pane_delta(&mut args, "-R")?,
                });
            }
            "-Z" => {
                let _ = args.optional();
                if adjustment.is_some() || absolute_width.is_some() || absolute_height.is_some() {
                    return Err(RmuxError::Server(
                        "resize-pane accepts only one adjustment flag".to_owned(),
                    ));
                }
                adjustment = Some(ResizePaneAdjustment::Zoom);
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
    args.no_extra("resize-pane")?;
    let adjustment = if trim_below {
        Some(ResizePaneAdjustment::TrimBelow)
    } else {
        adjustment.or(match (absolute_width, absolute_height) {
            (Some(columns), Some(rows)) => {
                Some(ResizePaneAdjustment::AbsoluteSize { columns, rows })
            }
            (Some(columns), None) => Some(ResizePaneAdjustment::AbsoluteWidth { columns }),
            (None, Some(rows)) => Some(ResizePaneAdjustment::AbsoluteHeight { rows }),
            (None, None) => None,
        })
    };

    Ok(Request::ResizePane(ResizePaneRequest {
        target: target.unwrap_or(implicit_pane_target(sessions, find_context, "resize-pane")?),
        adjustment: adjustment.unwrap_or(ResizePaneAdjustment::NoOp),
    }))
}

fn parse_resize_pane_delta(args: &mut CommandTokens, flag: &str) -> Result<u16, RmuxError> {
    match args.peek() {
        Some(next) if !next.starts_with('-') || next == "-" => parse_u16(
            "resize-pane",
            flag,
            &args.required(&format!("{flag} value"))?,
        ),
        _ => Ok(1),
    }
}
