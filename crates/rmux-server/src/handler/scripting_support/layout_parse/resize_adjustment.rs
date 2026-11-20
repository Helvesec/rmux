//! `resize-pane` relative adjustments: delta parsing, trailing values and composition with sizes.

use rmux_proto::{ResizePaneAdjustment, ResizePaneRelativeDirection, RmuxError};

use super::super::tokens::CommandTokens;

pub(super) fn parse_resize_pane_relative(
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

pub(super) fn parse_trailing_resize_pane_adjustment(
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

pub(super) fn parse_no_direction_trailing_resize_pane_adjustment(
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

pub(super) fn integer_like_resize_pane_adjustment(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn resize_pane_adjustment(
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
