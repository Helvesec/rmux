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

    Ok(Request::DisplayMessage(DisplayMessageRequest {
        target,
        print,
        message,
    }))
}

fn display_all_formats_template() -> String {
    DISPLAY_ALL_FORMATS
        .iter()
        .map(|name| match *name {
            "session_last_attached" => format!("{name}=#{{?{name},#{{{name}}},0}}"),
            _ => format!("{name}=#{{{name}}}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const DISPLAY_ALL_FORMATS: &[&str] = &[
    "active_window_index",
    "alternate_on",
    "alternate_saved_x",
    "alternate_saved_y",
    "buffer_mode_format",
    "client_mode_format",
    "config_files",
    "cursor_character",
    "cursor_flag",
    "cursor_x",
    "cursor_y",
    "history_all_bytes",
    "history_bytes",
    "history_limit",
    "history_size",
    "host",
    "host_short",
    "insert_flag",
    "keypad_cursor_flag",
    "keypad_flag",
    "last_window_index",
    "mouse_all_flag",
    "mouse_any_flag",
    "mouse_button_flag",
    "mouse_sgr_flag",
    "mouse_standard_flag",
    "mouse_utf8_flag",
    "next_session_id",
    "origin_flag",
    "pane_active",
    "pane_at_bottom",
    "pane_at_left",
    "pane_at_right",
    "pane_at_top",
    "pane_bg",
    "pane_bottom",
    "pane_current_command",
    "pane_current_path",
    "pane_dead",
    "pane_fg",
    "pane_format",
    "pane_height",
    "pane_id",
    "pane_in_mode",
    "pane_index",
    "pane_input_off",
    "pane_last",
    "pane_left",
    "pane_marked",
    "pane_marked_set",
    "pane_path",
    "pane_pid",
    "pane_pipe",
    "pane_right",
    "pane_search_string",
    "pane_start_command",
    "pane_start_path",
    "pane_synchronized",
    "pane_tabs",
    "pane_title",
    "pane_top",
    "pane_tty",
    "pane_unseen_changes",
    "pane_width",
    "pid",
    "scroll_region_lower",
    "scroll_region_upper",
    "server_sessions",
    "session_activity",
    "session_alerts",
    "session_attached",
    "session_created",
    "session_format",
    "session_grouped",
    "session_id",
    "session_last_attached",
    "session_many_attached",
    "session_marked",
    "session_name",
    "session_path",
    "session_stack",
    "session_windows",
    "socket_path",
    "start_time",
    "tree_mode_format",
    "uid",
    "user",
    "version",
    "window_active",
    "window_active_clients",
    "window_active_sessions",
    "window_active_sessions_list",
    "window_activity",
    "window_activity_flag",
    "window_bell_flag",
    "window_cell_height",
    "window_cell_width",
    "window_end_flag",
    "window_flags",
    "window_format",
    "window_height",
    "window_id",
    "window_index",
    "window_last_flag",
    "window_layout",
    "window_linked",
    "window_linked_sessions",
    "window_linked_sessions_list",
    "window_marked_flag",
    "window_name",
    "window_panes",
    "window_raw_flags",
    "window_silence_flag",
    "window_stack_index",
    "window_start_flag",
    "window_visible_layout",
    "window_width",
    "window_zoomed_flag",
    "wrap_flag",
    "command",
];

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
