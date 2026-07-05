use rmux_proto::request::{
    DetachClientExtRequest, ListClientsRequest, RefreshClientRequest, SuspendClientRequest,
    SwitchClientExt3Request,
};
use rmux_proto::{Request, RmuxError};

use super::parse_session_name;
use super::tokens::CommandTokens;
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

    while let Some(token) = args.peek() {
        match token {
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
            flag if flag.starts_with('-') => return Err(unsupported_flag("switch-client", flag)),
            _ => break,
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

    while let Some(token) = args.peek() {
        match token {
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
            flag if flag.starts_with('-') => return Err(unsupported_flag("detach-client", flag)),
            _ => break,
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
    let mut subscriptions = Vec::new();
    let mut subscriptions_format = Vec::new();
    let mut clear_pan = false;
    let mut control_size = None;
    let mut pan_down = false;
    let mut flags = None;
    let mut flags_alias = None;
    let mut clipboard_query = false;
    let mut pan_left = false;
    let mut pan_right = false;
    let mut colour_report = None;
    let mut status_only = false;
    let mut pan_up = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-A" => {
                let _ = args.optional();
                subscriptions.push(args.required("-A pane:state")?);
            }
            "-B" => {
                let _ = args.optional();
                subscriptions_format.push(args.required("-B name:pane:format")?);
            }
            "-c" => {
                let _ = args.optional();
                clear_pan = true;
            }
            "-C" => {
                let _ = args.optional();
                control_size = Some(args.required("-C widthxheight")?);
            }
            "-D" => {
                let _ = args.optional();
                pan_down = true;
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
            "-L" => {
                let _ = args.optional();
                pan_left = true;
            }
            "-R" => {
                let _ = args.optional();
                pan_right = true;
            }
            "-r" => {
                let _ = args.optional();
                colour_report = Some(args.required("-r pane:report")?);
            }
            "-S" => {
                let _ = args.optional();
                status_only = true;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            "-U" => {
                let _ = args.optional();
                pan_up = true;
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("refresh-client", flag)),
            _ => break,
        }
    }

    let adjustment = args
        .optional()
        .map(|value| {
            value.parse::<u32>().map_err(|_| {
                RmuxError::Server(format!("invalid refresh-client adjustment '{value}'"))
            })
        })
        .transpose()?;
    args.no_extra("refresh-client")?;

    Ok(Request::RefreshClient(Box::new(RefreshClientRequest {
        target_client,
        adjustment,
        clear_pan,
        pan_left,
        pan_right,
        pan_up,
        pan_down,
        status_only,
        clipboard_query,
        flags,
        flags_alias,
        subscriptions,
        subscriptions_format,
        control_size,
        colour_report,
    })))
}

pub(super) fn parse_list_clients(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut format = None;
    let mut filter = None;
    let mut sort_order = None;
    let mut reversed = false;
    let mut target_session = None;

    while let Some(token) = args.peek() {
        match token {
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
            flag if flag.starts_with('-') => return Err(unsupported_flag("list-clients", flag)),
            _ => break,
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
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
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
                let _ = args.optional();
                target = Some(args.required("-t target-pane")?);
            }
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
            flag if flag.starts_with('-') => return Err(unsupported_flag("server-access", flag)),
            _ => break,
        }
    }

    let user = args.optional();
    args.no_extra("server-access")?;

    Ok(Request::ServerAccess(rmux_proto::ServerAccessRequest {
        add,
        deny,
        list,
        read_only,
        write,
        target,
        user,
    }))
}
