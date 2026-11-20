//! `capture-pane` and `clear-history` parsing: the commands that read or reset a pane's history.

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{CapturePaneRequest, ClearHistoryRequest, Request, RmuxError};

use super::super::parse_pane_target;
use super::super::targets::implicit_pane_target;
use super::super::tokens::CommandTokens;
use super::super::values::unsupported_flag;

pub(in crate::handler::scripting_support) fn parse_capture_pane(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut start = None;
    let mut end = None;
    let mut print = false;
    let mut buffer_name = None;
    let mut alternate = false;
    let mut escape_ansi = false;
    let mut escape_sequences = false;
    let mut include_format = false;
    let mut hyperlinks = false;
    let mut line_numbers = false;
    let mut join_wrapped = false;
    let mut use_mode_screen = false;
    let mut do_not_trim_spaces = false;
    let mut preserve_trailing_spaces = false;
    let mut pending_input = false;
    let mut quiet = false;
    let mut start_is_absolute = false;
    let mut end_is_absolute = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-a" => alternate = true,
            "-e" => escape_ansi = true,
            "-C" => escape_sequences = true,
            "-F" => include_format = true,
            "-H" => hyperlinks = true,
            "-J" => join_wrapped = true,
            "-L" => line_numbers = true,
            "-M" => use_mode_screen = true,
            "-N" => do_not_trim_spaces = true,
            "-T" => preserve_trailing_spaces = true,
            "-P" => pending_input = true,
            "-q" => quiet = true,
            "-t" => {
                target = Some(parse_pane_target(
                    "capture-pane",
                    args.required("-t target")?,
                )?)
            }
            "-S" => {
                let value = args.required("-S value")?;
                if value == "-" {
                    start_is_absolute = true;
                } else {
                    start = Some(parse_capture_pane_bound("-S", &value)?);
                }
            }
            "-E" => {
                let value = args.required("-E value")?;
                if value == "-" {
                    end_is_absolute = true;
                } else {
                    end = Some(parse_capture_pane_bound("-E", &value)?);
                }
            }
            "-p" => print = true,
            "-b" => buffer_name = Some(args.required("-b buffer name")?),
            flag if flag.starts_with('-') => return Err(unsupported_flag("capture-pane", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for capture-pane"
                )));
            }
        }
    }

    Ok(Request::CapturePane(Box::new(CapturePaneRequest {
        target: target.unwrap_or(implicit_pane_target(
            sessions,
            find_context,
            "capture-pane",
        )?),
        start,
        end,
        print,
        buffer_name,
        alternate,
        escape_ansi,
        escape_sequences,
        include_format,
        hyperlinks,
        line_numbers,
        join_wrapped,
        use_mode_screen,
        preserve_trailing_spaces,
        do_not_trim_spaces,
        pending_input,
        quiet,
        start_is_absolute,
        end_is_absolute,
    })))
}

fn parse_capture_pane_bound(flag: &str, value: &str) -> Result<i64, RmuxError> {
    value
        .parse::<i64>()
        .map_err(|_| RmuxError::Server(format!("command capture-pane: {flag} expects a number")))
}

pub(in crate::handler::scripting_support) fn parse_clear_history(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut reset_hyperlinks = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-H" => reset_hyperlinks = true,
            "-t" => {
                target = Some(parse_pane_target(
                    "clear-history",
                    args.required("-t target")?,
                )?)
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("clear-history", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for clear-history"
                )));
            }
        }
    }

    Ok(Request::ClearHistory(ClearHistoryRequest {
        target: target.unwrap_or(implicit_pane_target(
            sessions,
            find_context,
            "clear-history",
        )?),
        reset_hyperlinks,
    }))
}
