use rmux_proto::{Request, RmuxError, SendKeysRequest};

use super::parse_pane_target;
use super::tokens::CommandTokens;
use super::values::{missing_argument, parse_usize};

pub(super) fn parse_send_keys(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut expand_formats = false;
    let mut hex = false;
    let mut literal = false;
    let mut dispatch_key_table = false;
    let mut copy_mode_command = false;
    let mut forward_mouse_event = false;
    let mut reset_terminal = false;
    let mut repeat_count = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                expand_formats = true;
            }
            "-H" => {
                let _ = args.optional();
                hex = true;
            }
            "-l" => {
                let _ = args.optional();
                literal = true;
            }
            "-K" => {
                let _ = args.optional();
                dispatch_key_table = true;
            }
            "-M" => {
                let _ = args.optional();
                forward_mouse_event = true;
            }
            "-N" => {
                let _ = args.optional();
                repeat_count = Some(parse_usize("send-keys", "-N", &args.required("-N count")?)?);
            }
            value if value.starts_with("-N") && value.len() > 2 => {
                let count = value[2..].to_owned();
                let _ = args.optional();
                repeat_count = Some(parse_usize("send-keys", "-N", &count)?);
            }
            "-R" => {
                let _ = args.optional();
                reset_terminal = true;
            }
            "-X" => {
                let _ = args.optional();
                copy_mode_command = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("send-keys", args.required("-t target")?)?);
            }
            _ => break,
        }
    }

    let keys = args.remaining();
    if target.is_some()
        && !expand_formats
        && !hex
        && !literal
        && !dispatch_key_table
        && !copy_mode_command
        && !forward_mouse_event
        && !reset_terminal
        && repeat_count.is_none()
    {
        return Ok(Request::SendKeys(SendKeysRequest {
            target: target.ok_or_else(|| missing_argument("send-keys", "-t target"))?,
            keys,
        }));
    }

    Ok(Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
        target,
        keys,
        expand_formats,
        hex,
        literal,
        dispatch_key_table,
        copy_mode_command,
        forward_mouse_event,
        reset_terminal,
        repeat_count,
    }))
}

pub(super) fn parse_bind_key(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut table_name = None;
    let mut note = None;
    let mut repeat = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-n" => {
                let _ = args.optional();
                table_name = Some("root".to_owned());
            }
            "-r" => {
                let _ = args.optional();
                repeat = true;
            }
            "-N" => {
                let _ = args.optional();
                note = Some(args.required("-N note")?);
            }
            "-T" => {
                let _ = args.optional();
                table_name = Some(args.required("-T key-table")?);
            }
            _ => break,
        }
    }

    let key = args.required("key")?;
    Ok(Request::BindKey(rmux_proto::BindKeyRequest {
        table_name: table_name.unwrap_or_else(|| "prefix".to_owned()),
        key,
        note,
        repeat,
        command: (!args.is_empty()).then_some(args.remaining()),
    }))
}

pub(super) fn parse_unbind_key(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut table_name = None;
    let mut all = false;
    let mut quiet = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                all = true;
            }
            "-n" => {
                let _ = args.optional();
                table_name = Some("root".to_owned());
            }
            "-q" => {
                let _ = args.optional();
                quiet = true;
            }
            "-T" => {
                let _ = args.optional();
                table_name = Some(args.required("-T key-table")?);
            }
            _ => break,
        }
    }

    let key = args.optional();
    args.no_extra("unbind-key")?;
    Ok(Request::UnbindKey(rmux_proto::UnbindKeyRequest {
        table_name: table_name.unwrap_or_else(|| "prefix".to_owned()),
        all,
        key,
        quiet,
    }))
}

pub(super) fn parse_list_keys(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut table_name = None;
    let mut first_only = false;
    let mut include_unnoted = false;
    let mut notes = false;
    let mut reversed = false;
    let mut format = None;
    let mut sort_order = None;
    let mut prefix = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-1" => {
                let _ = args.optional();
                first_only = true;
            }
            "-a" => {
                let _ = args.optional();
                include_unnoted = true;
            }
            "-N" => {
                let _ = args.optional();
                notes = true;
            }
            "-r" => {
                let _ = args.optional();
                reversed = true;
            }
            "-F" => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            "-O" => {
                let _ = args.optional();
                sort_order = Some(args.required("-O sort-order")?);
            }
            "-P" => {
                let _ = args.optional();
                prefix = Some(args.required("-P prefix")?);
            }
            "-T" => {
                let _ = args.optional();
                table_name = Some(args.required("-T key-table")?);
            }
            _ => break,
        }
    }

    let key = args.optional();
    args.no_extra("list-keys")?;
    Ok(Request::ListKeys(rmux_proto::ListKeysRequest {
        table_name,
        first_only,
        notes,
        include_unnoted,
        reversed,
        format,
        sort_order,
        prefix,
        key,
    }))
}

pub(super) fn parse_send_prefix(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut secondary = false;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-2" => {
                let _ = args.optional();
                secondary = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target(
                    "send-prefix",
                    args.required("-t target")?,
                )?);
            }
            _ => break,
        }
    }
    args.no_extra("send-prefix")?;
    Ok(Request::SendPrefix(rmux_proto::SendPrefixRequest {
        target,
        secondary,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> String {
        value.to_owned()
    }

    #[test]
    fn parse_send_keys_accepts_tmux_compact_repeat_count() {
        let request = parse_send_keys(CommandTokens::new(vec![
            token("-N5"),
            token("-X"),
            token("scroll-up"),
        ]))
        .expect("compact repeat send-keys parses");

        let Request::SendKeysExt(request) = request else {
            panic!("compact repeat must use extended send-keys request");
        };
        assert_eq!(request.repeat_count, Some(5));
        assert!(request.copy_mode_command);
        assert_eq!(request.keys, vec!["scroll-up"]);
    }
}
