use rmux_core::{
    CommandTargetSpec, OptionStore, SessionStore, TargetFindContext, TargetFindFlags,
    TargetFindType,
};
use rmux_proto::{
    LinkWindowRequest, MoveWindowRequest, MoveWindowTarget, OptionName, Request, RmuxError,
    SwapWindowRequest, Target, UnlinkWindowRequest, WindowTarget,
};

use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::unsupported_flag;
use super::super::{
    implicit_session_name, implicit_window_target, marked_pane_target, parse_move_window_target,
    parse_window_target, resolve_target_argument_with_spec,
};
use crate::handler::scripting_support::targets::bare_link_window_destination_candidate;

pub(in crate::handler::scripting_support) fn parse_move_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut renumber = false;
    let mut kill_destination = false;
    let mut detached = false;
    let mut after = false;
    let mut before = false;
    let mut source = None;
    let mut target_value = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-r" => {
                let _ = args.optional();
                renumber = true;
            }
            "-k" => {
                let _ = args.optional();
                kill_destination = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-a" => {
                let _ = args.optional();
                if before {
                    return Err(RmuxError::Server(
                        "move-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                if after {
                    return Err(RmuxError::Server(
                        "move-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                before = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "move-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target_value = Some(args.required("-t target")?);
            }
            _ => break,
        }
    }
    args.no_extra("move-window")?;

    if renumber {
        if after || before {
            return Err(RmuxError::Server(
                "move-window -r does not accept -a or -b".to_owned(),
            ));
        }
        if kill_destination {
            return Err(RmuxError::Server(
                "move-window -r does not accept -k".to_owned(),
            ));
        }
        let mut target = target_value.map(parse_move_window_target).transpose()?;
        if target.is_none() {
            target = Some(MoveWindowTarget::Session(implicit_session_name(
                sessions,
                find_context,
                "move-window",
            )?));
        }
        return Ok(Request::MoveWindow(MoveWindowRequest {
            source: None,
            target: target.expect("validated move-window target"),
            renumber,
            kill_destination,
            detached,
            after,
            before,
        }));
    }
    if source.is_some() {
        renumber = false;
    }

    let resolved_source = source.unwrap_or(implicit_window_target(
        sessions,
        find_context,
        "move-window",
    )?);
    let resolved_target = match target_value {
        Some(value) => parse_move_window_destination_target(
            value,
            after,
            before,
            sessions,
            options,
            find_context,
        )?,
        None if after || before => MoveWindowTarget::Window(implicit_window_target(
            sessions,
            find_context,
            "move-window",
        )?),
        None => {
            let destination_session = implicit_session_name(sessions, find_context, "move-window")?;
            MoveWindowTarget::Window(first_available_window_target(
                sessions,
                options,
                &destination_session,
                "move-window",
            )?)
        }
    };
    Ok(Request::MoveWindow(MoveWindowRequest {
        source: Some(resolved_source),
        target: resolved_target,
        renumber,
        kill_destination,
        detached,
        after,
        before,
    }))
}

fn first_available_window_target(
    sessions: &SessionStore,
    options: &OptionStore,
    session_name: &rmux_proto::SessionName,
    command_name: &str,
) -> Result<WindowTarget, RmuxError> {
    let session = sessions
        .session(session_name)
        .ok_or_else(|| crate::pane_terminals::session_not_found(session_name))?;
    let base_index = options
        .resolve(Some(session_name), OptionName::BaseIndex)
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let mut index = base_index;
    loop {
        if !session.windows().contains_key(&index) {
            return Ok(WindowTarget::with_window(session_name.clone(), index));
        }
        index = index.checked_add(1).ok_or_else(|| {
            RmuxError::Server(format!("{command_name}: window index space exhausted"))
        })?;
    }
}

pub(in crate::handler::scripting_support) fn parse_link_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut after = false;
    let mut before = false;
    let mut detached = false;
    let mut kill_destination = false;
    let mut source = None;
    let mut target_value = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                if before {
                    return Err(RmuxError::Server(
                        "link-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                if after {
                    return Err(RmuxError::Server(
                        "link-window accepts only one of -a or -b".to_owned(),
                    ));
                }
                before = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-k" => {
                let _ = args.optional();
                kill_destination = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "link-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target_value = Some(args.required("-t target")?);
            }
            _ => break,
        }
    }
    args.no_extra("link-window")?;
    let source = source.unwrap_or(implicit_window_target(
        sessions,
        find_context,
        "link-window",
    )?);
    let target = match target_value {
        Some(target_value) => {
            parse_link_window_target(target_value, after, before, sessions, options, find_context)?
        }
        None if after || before => implicit_window_target(sessions, find_context, "link-window")?,
        None => {
            let session_name = implicit_session_name(sessions, find_context, "link-window")?;
            first_available_window_target(sessions, options, &session_name, "link-window")?
        }
    };

    Ok(Request::LinkWindow(LinkWindowRequest {
        source,
        target,
        after,
        before,
        kill_destination,
        detached,
    }))
}

fn parse_link_window_target(
    value: String,
    after: bool,
    before: bool,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<WindowTarget, RmuxError> {
    parse_link_or_move_window_target(
        "link-window",
        value,
        after,
        before,
        sessions,
        options,
        find_context,
    )
}

fn parse_move_window_destination_target(
    value: String,
    after: bool,
    before: bool,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<MoveWindowTarget, RmuxError> {
    if after || before {
        return parse_window_placement_target("move-window", value, sessions, find_context)
            .map(MoveWindowTarget::Window);
    }
    if let Some(session_name) = parse_session_only_link_destination(&value, sessions, find_context)?
    {
        return Ok(MoveWindowTarget::Session(session_name));
    }
    parse_link_or_move_window_target(
        "move-window",
        value,
        false,
        false,
        sessions,
        options,
        find_context,
    )
    .map(MoveWindowTarget::Window)
}

fn parse_link_or_move_window_target(
    command_name: &str,
    value: String,
    after: bool,
    before: bool,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<WindowTarget, RmuxError> {
    if after || before {
        return parse_window_placement_target(command_name, value, sessions, find_context);
    }

    if let Some(session_name) = parse_session_only_link_destination(&value, sessions, find_context)?
    {
        return first_available_window_target(sessions, options, &session_name, command_name);
    }
    if let Some(target) = exact_current_session_window_name_target(&value, sessions, find_context) {
        return Ok(target);
    }
    if let Some(target) =
        parse_bare_session_link_destination(command_name, &value, sessions, options, find_context)?
    {
        return Ok(target);
    }

    let resolved = resolve_target_argument_with_spec(
        value,
        CommandTargetSpec {
            flag: 't',
            find_type: TargetFindType::Window,
            flags: TargetFindFlags::WINDOW_INDEX,
        },
        sessions,
        find_context,
    )?;
    parse_window_target(command_name, resolved)
}

fn parse_bare_session_link_destination(
    command_name: &str,
    value: &str,
    sessions: &SessionStore,
    options: &OptionStore,
    find_context: &TargetFindContext,
) -> Result<Option<WindowTarget>, RmuxError> {
    if !bare_link_window_destination_candidate(value) {
        return Ok(None);
    }
    let session_name = match resolve_session_name_argument(value.to_owned(), sessions, find_context)
    {
        Ok(session_name) => session_name,
        Err(_) => return Ok(None),
    };
    first_available_window_target(sessions, options, &session_name, command_name).map(Some)
}

fn parse_window_placement_target(
    command_name: &str,
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<WindowTarget, RmuxError> {
    if let Some(session_value) = signed_window_target_session_part(&value) {
        if let Some(session_value) = session_value {
            let session_name =
                resolve_session_name_argument(session_value.to_owned(), sessions, find_context)?;
            let window_index = sessions
                .session(&session_name)
                .ok_or_else(|| crate::pane_terminals::session_not_found(&session_name))?
                .active_window_index();
            return Ok(WindowTarget::with_window(session_name, window_index));
        }
        return implicit_window_target(sessions, find_context, command_name);
    }

    let resolved = resolve_target_argument_with_spec(
        value,
        CommandTargetSpec {
            flag: 't',
            find_type: TargetFindType::Window,
            flags: TargetFindFlags::NONE,
        },
        sessions,
        find_context,
    )?;
    parse_window_target(command_name, resolved)
}

fn parse_session_only_link_destination(
    value: &str,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Option<rmux_proto::SessionName>, RmuxError> {
    if let Some(session) = value
        .strip_suffix(':')
        .filter(|session| !session.is_empty())
    {
        return resolve_session_name_argument(session.to_owned(), sessions, find_context).map(Some);
    }
    Ok(None)
}

fn resolve_session_name_argument(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<rmux_proto::SessionName, RmuxError> {
    let resolved = resolve_target_argument_with_spec(
        value,
        CommandTargetSpec {
            flag: 't',
            find_type: TargetFindType::Session,
            flags: TargetFindFlags::NONE,
        },
        sessions,
        find_context,
    )?;
    match Target::parse(&resolved)? {
        Target::Session(session_name) => Ok(session_name),
        _ => unreachable!("session target lookup must return a session"),
    }
}

fn exact_current_session_window_name_target(
    value: &str,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Option<WindowTarget> {
    if !bare_link_window_destination_candidate(value) {
        return None;
    }
    let session_name = find_context.current()?.session_name();
    let session = sessions.session(session_name)?;
    let mut matches = session
        .windows()
        .iter()
        .filter_map(|(window_index, window)| (window.name()? == value).then_some(*window_index));
    let window_index = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(WindowTarget::with_window(
        session_name.clone(),
        window_index,
    ))
}

fn signed_window_target_session_part(raw_target: &str) -> Option<Option<&str>> {
    if signed_window_index_target(raw_target) {
        return Some(None);
    }
    let (session, window) = raw_target.split_once(':')?;
    if session.is_empty() || !signed_window_index_target(window) {
        return None;
    }
    Some(Some(session))
}

fn signed_window_index_target(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.bytes().all(|byte| byte.is_ascii_digit())
}

pub(in crate::handler::scripting_support) fn parse_swap_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut detached = false;
    let mut source = None;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-s" => {
                let _ = args.optional();
                source = Some(parse_window_target(
                    "swap-window",
                    args.required("-s target")?,
                )?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(
                    "swap-window",
                    args.required("-t target")?,
                )?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "d", "st") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('d') => detached = true,
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            source = Some(parse_window_target(
                                "swap-window",
                                compact_flag.value_or_next(&mut args, "-s target")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_window_target(
                                "swap-window",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("swap-window", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    args.no_extra("swap-window")?;

    let source = match source {
        Some(source) => source,
        None => marked_pane_target(sessions, find_context, "swap-window")
            .map(|marked| {
                WindowTarget::with_window(marked.session_name().clone(), marked.window_index())
            })
            .or_else(|_| implicit_window_target(sessions, find_context, "swap-window"))?,
    };

    Ok(Request::SwapWindow(SwapWindowRequest {
        source,
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "swap-window",
        )?),
        detached,
    }))
}

pub(in crate::handler::scripting_support) fn parse_unlink_window(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut kill_if_last = false;
    let mut target = None;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-k" => kill_if_last = true,
            "-t" => {
                target = Some(parse_window_target(
                    "unlink-window",
                    args.required("-t target")?,
                )?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("unlink-window", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for unlink-window"
                )));
            }
        }
    }

    Ok(Request::UnlinkWindow(UnlinkWindowRequest {
        target: target.unwrap_or(implicit_window_target(
            sessions,
            find_context,
            "unlink-window",
        )?),
        kill_if_last,
    }))
}
