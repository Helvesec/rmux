use rmux_core::formats::TMUX_FORMAT_TABLE_NAMES;
use rmux_proto::{
    CapturePaneRequest, ClearHistoryRequest, DisplayMessageRequest, Request, RmuxError,
    ShowMessagesRequest,
};

use super::tokens::CommandTokens;
use super::values::{missing_argument, parse_i64, unsupported_flag};
use super::{parse_pane_target, parse_target_arg};

pub(super) fn parse_capture_pane(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut start = None;
    let mut end = None;
    let mut print = false;
    let mut buffer_name = None;
    let mut alternate = false;
    let mut escape_ansi = false;
    let mut escape_sequences = false;
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
            "-J" => join_wrapped = true,
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
                    start = Some(parse_i64("capture-pane", "-S", &value)?);
                }
            }
            "-E" => {
                let value = args.required("-E value")?;
                if value == "-" {
                    end_is_absolute = true;
                } else {
                    end = Some(parse_i64("capture-pane", "-E", &value)?);
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

    Ok(Request::CapturePane(CapturePaneRequest {
        target: target.ok_or_else(|| missing_argument("capture-pane", "-t target"))?,
        start,
        end,
        print,
        buffer_name,
        alternate,
        escape_ansi,
        escape_sequences,
        join_wrapped,
        use_mode_screen,
        preserve_trailing_spaces,
        do_not_trim_spaces,
        pending_input,
        quiet,
        start_is_absolute,
        end_is_absolute,
    }))
}

pub(super) fn parse_clear_history(mut args: CommandTokens) -> Result<Request, RmuxError> {
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
        target: target.ok_or_else(|| missing_argument("clear-history", "-t target"))?,
        reset_hyperlinks,
    }))
}

pub(super) fn parse_display_message(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut target_client = None;
    let mut print = false;
    let mut all_formats = false;
    let mut message = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                message = Some(args.required("-F format")?);
            }
            "-a" => {
                let _ = args.optional();
                all_formats = true;
                print = true;
            }
            "-c" => {
                let _ = args.optional();
                target_client = Some(args.required("-c target-client")?);
            }
            "-d" => {
                let _ = args.optional();
                let _ = args.required("-d delay")?;
            }
            "-I" | "-l" | "-N" | "-v" => {
                let _ = args.optional();
            }
            "-p" => {
                let _ = args.optional();
                print = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg(
                    "display-message",
                    args.required("-t target")?,
                )?)
            }
            _ => break,
        }
    }

    if all_formats {
        message = Some(display_all_formats_template());
        args.no_extra("display-message")?;
    } else if message.is_none() && !args.is_empty() {
        message = Some(args.remaining_joined());
    } else {
        args.no_extra("display-message")?;
    }

    if target_client.is_some() {
        return Ok(Request::DisplayMessageExt(
            rmux_proto::DisplayMessageExtRequest {
                target,
                print,
                message,
                target_client,
            },
        ));
    }

    Ok(Request::DisplayMessage(DisplayMessageRequest {
        target,
        print,
        message,
    }))
}

fn display_all_formats_template() -> String {
    TMUX_FORMAT_TABLE_NAMES
        .iter()
        .copied()
        .chain(DISPLAY_ALL_EXTRA_FORMATS.iter().copied())
        .map(|name| match name {
            "session_last_attached" => format!("{name}=#{{?{name},#{{{name}}},0}}"),
            _ => format!("{name}=#{{{name}}}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const DISPLAY_ALL_EXTRA_FORMATS: &[&str] = &["command"];

pub(super) fn parse_show_messages(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut jobs = false;
    let mut terminals = false;
    let mut target_client = None;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-J" => jobs = true,
            "-T" => terminals = true,
            "-t" => target_client = Some(args.required("-t target-client")?),
            flag if flag.starts_with('-') => return Err(unsupported_flag("show-messages", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for show-messages"
                )));
            }
        }
    }

    Ok(Request::ShowMessages(ShowMessagesRequest {
        jobs,
        terminals,
        target_client,
    }))
}
