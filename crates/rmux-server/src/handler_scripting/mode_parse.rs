use rmux_proto::{Request, RmuxError};

use super::parse_pane_target;
use super::tokens::CommandTokens;
use super::values::unsupported_flag;

pub(super) fn parse_copy_mode(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut source = None;
    let page_down = false;
    let mut exit_on_scroll = false;
    let mut hide_position = false;
    let mut mouse_drag_start = false;
    let mut cancel_mode = false;
    let scrollbar_scroll = false;
    let mut page_up = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-d" => return Err(unsupported_flag("copy-mode", "-d")),
            "-e" => {
                let _ = args.optional();
                exit_on_scroll = true;
            }
            "-H" => {
                let _ = args.optional();
                hide_position = true;
            }
            "-M" => {
                let _ = args.optional();
                mouse_drag_start = true;
            }
            "-q" => {
                let _ = args.optional();
                cancel_mode = true;
            }
            "-S" => return Err(unsupported_flag("copy-mode", "-S")),
            "-s" => {
                let _ = args.optional();
                source = Some(parse_pane_target(
                    "copy-mode",
                    args.required("-s src-pane")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("copy-mode", args.required("-t target")?)?);
            }
            "-u" => {
                let _ = args.optional();
                page_up = true;
            }
            _ => break,
        }
    }

    args.no_extra("copy-mode")?;
    Ok(Request::CopyMode(rmux_proto::CopyModeRequest {
        target,
        page_down,
        exit_on_scroll,
        hide_position,
        mouse_drag_start,
        cancel_mode,
        scrollbar_scroll,
        source,
        page_up,
    }))
}

pub(super) fn parse_clock_mode(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "clock-mode",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }

    args.no_extra("clock-mode")?;
    Ok(Request::ClockMode(rmux_proto::ClockModeRequest { target }))
}
