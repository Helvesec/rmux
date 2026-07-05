use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::request::{AttachSessionExt2Request, NewSessionExtRequest};
use rmux_proto::{
    ClientTerminalContext, DisplayPanesRequest, HasSessionRequest, KillSessionRequest,
    LastWindowRequest, LockSessionRequest, NextWindowRequest, PreviousWindowRequest,
    RenameSessionRequest, Request, RmuxError, SwitchClientRequest, TerminalSize,
};

use super::super::DEFAULT_SESSION_SIZE;
use super::tokens::CommandTokens;
use super::values::{parse_u16, unsupported_flag};
use super::{implicit_session_name, parse_session_name};

pub(super) fn parse_new_session(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut attach_if_exists = false;
    let mut detach_other_clients = false;
    let mut detached = false;
    let mut environment = Vec::new();
    let mut flags = Vec::new();
    let mut group_target = None;
    let mut kill_other_clients = false;
    let mut print_format = None;
    let mut print_session_info = false;
    let mut session_name = None;
    let mut skip_environment_update = false;
    let mut working_directory = None;
    let mut window_name = None;
    let mut cols = None;
    let mut rows = None;
    let mut command = Vec::new();

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-A" => attach_if_exists = true,
            "-c" => working_directory = Some(args.required("-c start-directory")?),
            "-D" => detach_other_clients = true,
            "-d" => detached = true,
            "-E" => skip_environment_update = true,
            "-e" => environment.push(args.required("-e name=value")?),
            "-f" => flags.push(args.required("-f flags")?),
            "-F" => print_format = Some(args.required("-F format")?),
            "-n" => window_name = Some(args.required("-n window-name")?),
            "-P" => print_session_info = true,
            "-s" => session_name = Some(parse_session_name(args.required("-s session")?)?),
            "-t" => group_target = Some(parse_session_name(args.required("-t group")?)?),
            "-X" => kill_other_clients = true,
            "-x" => cols = Some(parse_u16("new-session", "-x", &args.required("-x value")?)?),
            "-y" => rows = Some(parse_u16("new-session", "-y", &args.required("-y value")?)?),
            "--" => {
                command.extend(args.remaining());
                break;
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("new-session", flag)),
            _ => {
                command.push(token);
                command.extend(args.remaining());
                break;
            }
        }
    }

    let size = match (cols, rows) {
        (None, None) => None,
        (cols, rows) => Some(TerminalSize {
            cols: cols.unwrap_or(DEFAULT_SESSION_SIZE.cols),
            rows: rows.unwrap_or(DEFAULT_SESSION_SIZE.rows),
        }),
    };

    if group_target.is_some() && (window_name.is_some() || !command.is_empty()) {
        return Err(RmuxError::Server(
            "command or window name given with target".to_owned(),
        ));
    }

    Ok(Request::NewSessionExt(Box::new(NewSessionExtRequest {
        detached,
        size,
        environment: (!environment.is_empty()).then_some(environment),
        session_name,
        working_directory,
        group_target,
        attach_if_exists,
        detach_other_clients: detach_other_clients || kill_other_clients,
        kill_other_clients,
        flags: (!flags.is_empty()).then_some(flags),
        window_name,
        print_session_info,
        print_format,
        command: (!command.is_empty()).then_some(command),
        process_command: None,
        client_environment: None,
        skip_environment_update,
    })))
}

pub(super) fn parse_session_request(
    mut args: CommandTokens,
    command: &str,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut alerts_only = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-a" if matches!(command, "next-window" | "previous-window") => alerts_only = true,
            "-t" => target = Some(parse_session_name(args.required("-t target")?)?),
            flag if flag.starts_with('-') => return Err(unsupported_flag(command, flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for {command}"
                )));
            }
        }
    }

    let target = target.unwrap_or(implicit_session_name(sessions, find_context, command)?);
    match command {
        "has-session" => Ok(Request::HasSession(HasSessionRequest { target })),
        "next-window" => Ok(Request::NextWindow(NextWindowRequest {
            target,
            alerts_only,
        })),
        "previous-window" => Ok(Request::PreviousWindow(PreviousWindowRequest {
            target,
            alerts_only,
        })),
        "last-window" => Ok(Request::LastWindow(LastWindowRequest { target })),
        "display-panes" => Ok(Request::DisplayPanes(DisplayPanesRequest {
            target,
            duration_ms: None,
            non_blocking: false,
            no_command: false,
            template: None,
        })),
        "switch-client" => Ok(Request::SwitchClient(SwitchClientRequest { target })),
        "lock-session" => Ok(Request::LockSession(LockSessionRequest { target })),
        _ => Err(RmuxError::Server(format!(
            "unsupported session request parser command: {command}"
        ))),
    }
}

pub(super) fn parse_kill_session(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut kill_all_except_target = false;
    let mut clear_alerts = false;
    let mut kill_group = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-a" => kill_all_except_target = true,
            "-C" => clear_alerts = true,
            "-g" => kill_group = true,
            "-t" => target = Some(parse_session_name(args.required("-t target")?)?),
            flag if flag.starts_with('-') => return Err(unsupported_flag("kill-session", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for kill-session"
                )));
            }
        }
    }

    Ok(Request::KillSession(KillSessionRequest {
        target: target.unwrap_or(implicit_session_name(
            sessions,
            find_context,
            "kill-session",
        )?),
        kill_all_except_target,
        clear_alerts,
        kill_group,
    }))
}

pub(super) fn parse_rename_session(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_session_name(args.required("-t target")?)?);
            }
            _ => break,
        }
    }

    let new_name = parse_session_name(args.required("rename-session new-name")?)?;
    args.no_extra("rename-session")?;

    Ok(Request::RenameSession(RenameSessionRequest {
        target: target.unwrap_or(implicit_session_name(
            sessions,
            find_context,
            "rename-session",
        )?),
        new_name,
    }))
}

pub(super) fn parse_attach_session(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut target_spec = None;
    let mut working_directory = None;
    let mut detach_other_clients = false;
    let mut kill_other_clients = false;
    let mut read_only = false;
    let mut skip_environment_update = false;
    let mut flags = Vec::new();

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-c" => working_directory = Some(args.required("-c working-directory")?),
            "-d" => detach_other_clients = true,
            "-E" => skip_environment_update = true,
            "-f" => flags.push(args.required("-f flags")?),
            "-r" => read_only = true,
            "-t" => {
                let value = args.required("-t target")?;
                target = Some(parse_session_name(value.clone())?);
                target_spec = Some(value);
            }
            "-x" => kill_other_clients = true,
            flag if flag.starts_with('-') => return Err(unsupported_flag("attach-session", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for attach-session"
                )));
            }
        }
    }

    Ok(Request::AttachSessionExt2(Box::new(
        AttachSessionExt2Request {
            target,
            target_spec,
            detach_other_clients: detach_other_clients || kill_other_clients,
            kill_other_clients,
            read_only,
            skip_environment_update,
            flags: (!flags.is_empty()).then_some(flags),
            working_directory,
            client_terminal: ClientTerminalContext::default(),
            client_size: None,
        },
    )))
}
