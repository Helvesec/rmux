use rmux_proto::request::{
    DetachClientExtRequest, ListClientsRequest, RefreshClientRequest, SuspendClientRequest,
    SwitchClientExt3Request,
};
use rmux_proto::{Request, RmuxError};

use super::parse_session_name;
use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::unsupported_flag;

pub(super) fn parse_switch_client(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut target_client = None;
    let mut key_table = None;
    let mut last_session = false;
    let mut next_session = false;
    let mut previous_session = false;
    let mut toggle_read_only = false;
    let mut sort_order = None;
    let mut skip_environment_update = false;
    let mut zoom = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-c" => {
                let _ = args.optional();
                target_client = Some(args.required("-c target-client")?);
            }
            "-E" => {
                let _ = args.optional();
                skip_environment_update = true;
            }
            "-l" => {
                let _ = args.optional();
                last_session = true;
            }
            "-n" => {
                let _ = args.optional();
                next_session = true;
            }
            "-O" => {
                let _ = args.optional();
                sort_order = Some(args.required("-O sort-order")?);
            }
            "-p" => {
                let _ = args.optional();
                previous_session = true;
            }
            "-r" => {
                let _ = args.optional();
                toggle_read_only = true;
            }
            "-T" => {
                let _ = args.optional();
                key_table = Some(args.required("-T key-table")?);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(args.required("-t target")?);
            }
            "-Z" => {
                let _ = args.optional();
                zoom = true;
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "ElnprZ", "cOTt") else {
                    if token.starts_with('-') {
                        return Err(unsupported_flag("switch-client", &token));
                    }
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('E') => skip_environment_update = true,
                        CompactFlag::Bare('l') => last_session = true,
                        CompactFlag::Bare('n') => next_session = true,
                        CompactFlag::Bare('p') => previous_session = true,
                        CompactFlag::Bare('r') => toggle_read_only = true,
                        CompactFlag::Bare('Z') => zoom = true,
                        compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-c target-client")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'O', .. } => {
                            sort_order =
                                Some(compact_flag.value_or_next(&mut args, "-O sort-order")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'T', .. } => {
                            key_table =
                                Some(compact_flag.value_or_next(&mut args, "-T key-table")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(compact_flag.value_or_next(&mut args, "-t target")?);
                        }
                        _ => unreachable!("compact switch-client flags are prevalidated"),
                    }
                }
            }
        }
    }
    args.no_extra("switch-client")?;

    let selector_count = usize::from(target.is_some())
        + usize::from(last_session)
        + usize::from(next_session)
        + usize::from(previous_session);
    if selector_count > 1 {
        return Err(RmuxError::Server(
            "switch-client accepts only one of -t, -l, -n, or -p".to_owned(),
        ));
    }

    Ok(Request::SwitchClientExt3(Box::new(
        SwitchClientExt3Request {
            target_client,
            target,
            key_table,
            last_session,
            next_session,
            previous_session,
            toggle_read_only,
            sort_order,
            skip_environment_update,
            zoom,
        },
    )))
}

pub(super) fn parse_detach_client(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target_client = None;
    let mut all_other_clients = false;
    let mut target_session = None;
    let mut kill_on_detach = false;
    let mut exec_command = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                all_other_clients = true;
            }
            "-E" => {
                let _ = args.optional();
                exec_command = Some(args.required("-E shell-command")?);
            }
            "-P" => {
                let _ = args.optional();
                kill_on_detach = true;
            }
            "-s" => {
                let _ = args.optional();
                target_session = Some(parse_session_name(args.required("-s target-session")?)?);
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "aP", "Est") else {
                    if token.starts_with('-') {
                        return Err(unsupported_flag("detach-client", &token));
                    }
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => all_other_clients = true,
                        CompactFlag::Bare('P') => kill_on_detach = true,
                        compact_flag @ CompactFlag::Value { flag: 'E', .. } => {
                            exec_command =
                                Some(compact_flag.value_or_next(&mut args, "-E shell-command")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 's', .. } => {
                            target_session = Some(parse_session_name(
                                compact_flag.value_or_next(&mut args, "-s target-session")?,
                            )?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-t target-client")?);
                        }
                        _ => unreachable!("compact detach-client flags are prevalidated"),
                    }
                }
            }
        }
    }
    args.no_extra("detach-client")?;
    Ok(Request::DetachClientExt(DetachClientExtRequest {
        target_client,
        all_other_clients,
        target_session,
        kill_on_detach,
        exec_command,
    }))
}

pub(super) fn parse_refresh_client(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target_client = None;
    let mut control_size = None;
    let mut flags = None;
    let mut flags_alias = None;
    let mut clipboard_query = false;
    let mut status_only = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-C" => {
                let _ = args.optional();
                control_size = Some(args.required("-C widthxheight")?);
            }
            "-f" => {
                let _ = args.optional();
                flags = Some(args.required("-f flags")?);
            }
            "-F" => {
                let _ = args.optional();
                flags_alias = Some(args.required("-F flags")?);
            }
            "-l" => {
                let _ = args.optional();
                clipboard_query = true;
            }
            "-S" => {
                let _ = args.optional();
                status_only = true;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "lS", "CfFt") else {
                    if token.starts_with('-') {
                        return Err(unsupported_flag("refresh-client", &token));
                    }
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('l') => clipboard_query = true,
                        CompactFlag::Bare('S') => status_only = true,
                        compact_flag @ CompactFlag::Value { flag: 'C', .. } => {
                            control_size =
                                Some(compact_flag.value_or_next(&mut args, "-C widthxheight")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'f', .. } => {
                            flags = Some(compact_flag.value_or_next(&mut args, "-f flags")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'F', .. } => {
                            flags_alias = Some(compact_flag.value_or_next(&mut args, "-F flags")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-t target-client")?);
                        }
                        _ => unreachable!("compact refresh-client flags are prevalidated"),
                    }
                }
            }
        }
    }

    args.no_extra("refresh-client")?;

    Ok(Request::RefreshClient(Box::new(RefreshClientRequest {
        target_client,
        adjustment: None,
        clear_pan: false,
        pan_left: false,
        pan_right: false,
        pan_up: false,
        pan_down: false,
        status_only,
        clipboard_query,
        flags,
        flags_alias,
        subscriptions: Vec::new(),
        subscriptions_format: Vec::new(),
        control_size,
        colour_report: None,
    })))
}

pub(super) fn parse_list_clients(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut format = None;
    let mut filter = None;
    let mut sort_order = None;
    let mut reversed = false;
    let mut target_session = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            "-f" => {
                let _ = args.optional();
                filter = Some(args.required("-f filter")?);
            }
            "-O" => {
                let _ = args.optional();
                sort_order = Some(args.required("-O order")?);
            }
            "-r" => {
                let _ = args.optional();
                reversed = true;
            }
            "-t" => {
                let _ = args.optional();
                target_session = Some(parse_session_name(args.required("-t target-session")?)?);
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "r", "FfOt") else {
                    if token.starts_with('-') {
                        return Err(unsupported_flag("list-clients", &token));
                    }
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('r') => reversed = true,
                        compact_flag @ CompactFlag::Value { flag: 'F', .. } => {
                            format = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'f', .. } => {
                            filter = Some(compact_flag.value_or_next(&mut args, "-f filter")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'O', .. } => {
                            sort_order = Some(compact_flag.value_or_next(&mut args, "-O order")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target_session = Some(parse_session_name(
                                compact_flag.value_or_next(&mut args, "-t target-session")?,
                            )?);
                        }
                        _ => unreachable!("compact list-clients flags are prevalidated"),
                    }
                }
            }
        }
    }
    args.no_extra("list-clients")?;
    Ok(Request::ListClients(Box::new(ListClientsRequest {
        format,
        filter,
        sort_order,
        reversed,
        target_session,
    })))
}

pub(super) fn parse_suspend_client(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target_client = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("suspend-client", flag)),
            _ => break,
        }
    }
    args.no_extra("suspend-client")?;
    Ok(Request::SuspendClient(SuspendClientRequest {
        target_client,
    }))
}

pub(super) fn parse_lock_client(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target_client = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            _ => break,
        }
    }

    args.no_extra("lock-client")?;
    Ok(Request::LockClient(rmux_proto::LockClientRequest {
        target_client: target_client.unwrap_or_else(|| "=".to_owned()),
    }))
}

pub(super) fn parse_server_access(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut add = false;
    let mut deny = false;
    let mut list = false;
    let mut read_only = false;
    let mut write = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                add = true;
            }
            "-d" => {
                let _ = args.optional();
                deny = true;
            }
            "-l" => {
                let _ = args.optional();
                list = true;
            }
            "-r" => {
                let _ = args.optional();
                read_only = true;
            }
            "-w" => {
                let _ = args.optional();
                write = true;
            }
            "-t" => {
                return Err(unsupported_flag("server-access", "-t"));
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "adlrw", "t") else {
                    match token.as_str() {
                        "--help" => return Err(unsupported_flag("server-access", "--help")),
                        "-" => {
                            return Err(RmuxError::Server(
                                "command server-access: invalid flag -".to_owned(),
                            ));
                        }
                        flag if flag.starts_with("--") => {
                            return Err(RmuxError::Server(
                                "command server-access: invalid flag --".to_owned(),
                            ));
                        }
                        flag if flag.starts_with('-') => {
                            return Err(unsupported_flag("server-access", flag));
                        }
                        _ => break,
                    }
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => add = true,
                        CompactFlag::Bare('d') => deny = true,
                        CompactFlag::Bare('l') => list = true,
                        CompactFlag::Bare('r') => read_only = true,
                        CompactFlag::Bare('w') => write = true,
                        CompactFlag::Value { flag: 't', .. } => {
                            return Err(unsupported_flag("server-access", "-t"));
                        }
                        _ => unreachable!("compact server-access flags are prevalidated"),
                    }
                }
            }
        }
    }

    let user = args.optional();
    args.no_extra("server-access")?;

    if !list && add && deny {
        return Err(RmuxError::Server(
            "-a and -d cannot be used together".to_owned(),
        ));
    }
    if !list && read_only && write {
        return Err(RmuxError::Server(
            "-r and -w cannot be used together".to_owned(),
        ));
    }

    Ok(Request::ServerAccess(rmux_proto::ServerAccessRequest {
        add,
        deny,
        list,
        read_only,
        write,
        target: None,
        user,
    }))
}
