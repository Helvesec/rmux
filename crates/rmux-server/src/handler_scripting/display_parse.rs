use rmux_core::formats::{
    format_skip_delimiter, DEFAULT_DISPLAY_MESSAGE_FORMAT, TMUX_FORMAT_TABLE_NAMES,
};
use rmux_core::{
    SessionStore, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{
    CapturePaneRequest, ClearHistoryRequest, DisplayMessageExtRequest, DisplayMessageRequest,
    PaneTarget, Request, RmuxError, ShowMessagesRequest, Target,
};

use super::targets::implicit_pane_target;
use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::unsupported_flag;
use super::{parse_pane_target, parse_target_arg};

pub(super) fn parse_capture_pane(
    mut args: CommandTokens,
    sessions: &SessionStore,
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

pub(super) fn parse_clear_history(
    mut args: CommandTokens,
    sessions: &SessionStore,
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

pub(super) fn parse_display_message(args: CommandTokens) -> Result<Request, RmuxError> {
    let parsed = parse_display_message_args(args)?;
    let target = parsed
        .target
        .map(|target| parse_target_arg("display-message", target))
        .transpose()?;

    Ok(display_message_request(
        target,
        parsed.print,
        parsed.message,
        parsed.target_client,
        false,
    ))
}

pub(super) fn parse_queued_display_message(
    args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    queue_current_target: Option<&Target>,
) -> Result<Request, RmuxError> {
    let parsed = parse_display_message_args(args)?;
    let (target, empty_target_context) = if parsed.stdin && parsed.target.is_none() {
        resolve_queued_display_message_stdin_target(sessions, find_context)
    } else {
        resolve_queued_display_message_target(
            parsed.target,
            sessions,
            find_context,
            queue_current_target,
        )?
    };
    if parsed.stdin && target.is_some() {
        return Err(RmuxError::Server("pane is not empty".to_owned()));
    }

    Ok(display_message_request(
        target,
        parsed.print,
        parsed.message,
        parsed.target_client,
        empty_target_context,
    ))
}

fn resolve_queued_display_message_stdin_target(
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> (Option<Target>, bool) {
    match sessions.resolve_unresolved_target(
        &UnresolvedTarget::none(),
        TargetFindType::Pane,
        TargetFindFlags::CANFAIL,
        find_context,
    ) {
        Ok(target) => (Some(target), false),
        Err(_) => (None, true),
    }
}

struct ParsedDisplayMessageArgs {
    target: Option<String>,
    target_client: Option<String>,
    print: bool,
    stdin: bool,
    message: Option<String>,
}

fn parse_display_message_args(
    mut args: CommandTokens,
) -> Result<ParsedDisplayMessageArgs, RmuxError> {
    let mut target = None;
    let mut target_client = None;
    let mut print = false;
    let mut stdin = false;
    let mut verbose = false;
    let mut all_formats = false;
    let mut no_expand = false;
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
                return Err(unsupported_flag("display-message", "-d"));
            }
            flag if flag.starts_with("-d") && flag.len() > 2 => {
                return Err(unsupported_flag("display-message", "-d"));
            }
            flag if is_display_message_compact_cluster(flag) => {
                let cluster = parse_compact_flag_cluster(flag, "aCIlNpv", "cdFt")
                    .expect("display-message compact cluster was prevalidated");
                let _ = args
                    .optional()
                    .expect("peeked display-message flag must still exist");
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => {
                            all_formats = true;
                            print = true;
                        }
                        CompactFlag::Bare('C') => {}
                        CompactFlag::Bare('I') => stdin = true,
                        CompactFlag::Bare('l') => no_expand = true,
                        CompactFlag::Bare('N') => {
                            return Err(unsupported_flag("display-message", "-N"));
                        }
                        CompactFlag::Bare('p') => print = true,
                        CompactFlag::Bare('v') => verbose = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("display-message", &format!("-{flag}")));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'F', .. } => {
                            message = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-c target-client")?);
                        }
                        CompactFlag::Value { flag: 'd', .. } => {
                            return Err(unsupported_flag("display-message", "-d"));
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(compact_flag.value_or_next(&mut args, "-t target")?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("display-message", &format!("-{flag}")));
                        }
                    }
                }
            }
            "-I" => {
                let _ = args.optional();
                stdin = true;
            }
            "-C" => {
                let _ = args.optional();
            }
            "-N" => {
                return Err(unsupported_flag("display-message", "-N"));
            }
            "-v" => {
                let _ = args.optional();
                verbose = true;
            }
            "-l" => {
                let _ = args.optional();
                no_expand = true;
            }
            "-p" => {
                let _ = args.optional();
                print = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(args.required("-t target")?)
            }
            _ => break,
        }
    }

    if all_formats {
        message = Some(display_all_formats_template());
        args.no_extra("display-message")?;
    } else if message.is_none() && !args.is_empty() {
        let remaining = args.remaining();
        match remaining.as_slice() {
            [single] => message = Some(single.clone()),
            _ => {
                return Err(RmuxError::Server(
                    "command display-message: too many arguments (need at most 1)".to_owned(),
                ));
            }
        }
    } else if message.is_some() && !args.is_empty() {
        return Err(RmuxError::Server(
            "only one of -F or argument must be given".to_owned(),
        ));
    } else {
        args.no_extra("display-message")?;
    }

    if no_expand {
        message = message.map(|value| literal_display_message_template(&value));
    }
    if verbose {
        let template = message.as_deref().unwrap_or(DEFAULT_DISPLAY_MESSAGE_FORMAT);
        message = Some(verbose_display_message_template(template, print));
        print = true;
    }

    Ok(ParsedDisplayMessageArgs {
        target,
        target_client,
        print,
        stdin,
        message,
    })
}

fn resolve_queued_display_message_target(
    target: Option<String>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
    queue_current_target: Option<&Target>,
) -> Result<(Option<Target>, bool), RmuxError> {
    let Some(target) = target else {
        return Ok((queue_current_target.cloned(), false));
    };
    let explicit_mouse_target = matches!(target.as_str(), "=" | "{mouse}");
    match sessions.resolve_unresolved_target(
        &UnresolvedTarget::new(target.clone()),
        TargetFindType::Pane,
        TargetFindFlags::CANFAIL,
        find_context,
    ) {
        Ok(target) => Ok((Some(target), false)),
        Err(error) if explicit_mouse_target => Err(error),
        Err(_) if target.contains(':') => {
            if let Some(target) =
                display_message_session_window_fallback(&target, sessions, find_context)
            {
                Ok((Some(target), false))
            } else if queue_current_target.is_some() {
                sessions
                    .resolve_unresolved_target(
                        &UnresolvedTarget::none(),
                        TargetFindType::Pane,
                        TargetFindFlags::NONE,
                        find_context,
                    )
                    .map(|target| (Some(target), false))
            } else {
                Ok((None, true))
            }
        }
        Err(_) if queue_current_target.is_some() => sessions
            .resolve_unresolved_target(
                &UnresolvedTarget::none(),
                TargetFindType::Pane,
                TargetFindFlags::NONE,
                find_context,
            )
            .map(|target| (Some(target), false)),
        Err(_) => Ok((None, true)),
    }
}

fn display_message_session_window_fallback(
    target: &str,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Option<Target> {
    let (session_part, window_part) = target.split_once(':')?;
    if session_part.is_empty() {
        return None;
    }
    let Target::Session(session_name) = sessions
        .resolve_unresolved_target(
            &UnresolvedTarget::new(session_part.to_owned()),
            TargetFindType::Session,
            TargetFindFlags::NONE,
            find_context,
        )
        .ok()?
    else {
        return None;
    };
    let session = sessions.session(&session_name)?;
    let window_index = display_message_fallback_window_index(session, window_part);
    let window = session.window_at(window_index)?;
    Some(Target::Pane(PaneTarget::with_window(
        session_name,
        window_index,
        window.active_pane_index(),
    )))
}

fn display_message_fallback_window_index(session: &rmux_core::Session, window_part: &str) -> u32 {
    if window_part.is_empty() {
        return session.active_window_index();
    }
    session
        .windows()
        .iter()
        .find_map(|(index, window)| {
            window
                .name()
                .is_some_and(|name| name == window_part)
                .then_some(*index)
        })
        .or_else(|| {
            session.windows().iter().find_map(|(index, window)| {
                window
                    .name()
                    .is_some_and(|name| name.starts_with(window_part))
                    .then_some(*index)
            })
        })
        .or_else(|| {
            session.windows().iter().find_map(|(index, window)| {
                window
                    .name()
                    .is_some_and(|name| rmux_core::fnmatch(window_part, name))
                    .then_some(*index)
            })
        })
        .unwrap_or_else(|| session.active_window_index())
}

fn display_message_request(
    target: Option<Target>,
    print: bool,
    message: Option<String>,
    target_client: Option<String>,
    empty_target_context: bool,
) -> Request {
    if target_client.is_some() {
        return Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
            target,
            print,
            message,
            target_client,
            empty_target_context,
        }));
    }

    Request::DisplayMessage(DisplayMessageRequest {
        target,
        print,
        message,
        empty_target_context,
    })
}

fn literal_display_message_template(value: &str) -> String {
    value.replace('#', "##")
}

fn verbose_display_message_template(template: &str, print_result: bool) -> String {
    let mut lines = vec![literal_display_message_template(&format!(
        "# expanding format: {template}"
    ))];
    for token in verbose_format_tokens(template) {
        lines.push(literal_display_message_template(&format!(
            "# found #{{}}: {token}"
        )));
        lines.push(format!(
            "{}#{{{token}}}",
            literal_display_message_template(&format!("# format '{token}' found: "))
        ));
        lines.push(format!(
            "{}#{{{token}}}{}",
            literal_display_message_template(&format!("# replaced '{token}' with '")),
            literal_display_message_template("'")
        ));
    }
    lines.push(format!(
        "{}{template}",
        literal_display_message_template("# result is: ")
    ));
    if print_result {
        lines.push(template.to_owned());
    }
    lines.join("\n")
}

fn verbose_format_tokens(template: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = template.as_bytes();
    let mut index = 0;
    while index + 1 < bytes.len() {
        if bytes[index] == b'#' && bytes[index + 1] == b'{' {
            let body_start = index + 2;
            let Some(end_offset) = format_skip_delimiter(&template[index..], b"}") else {
                break;
            };
            let body_end = index + end_offset;
            let token = &template[body_start..body_end];
            if verbose_token_is_simple_variable(token) {
                tokens.push(token.to_owned());
            }
            index = body_end + 1;
        } else {
            index += 1;
        }
    }
    tokens
}

fn verbose_token_is_simple_variable(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_display_message_compact_cluster(flag: &str) -> bool {
    parse_compact_flag_cluster(flag, "aCIlNpv", "cdFt").is_some()
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
