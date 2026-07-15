use rmux_proto::{Request, RmuxError, SendKeysRequest};

use super::parse_pane_target;
use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{
    missing_argument, parse_usize, reject_unknown_option_before_positional, unsupported_flag,
};

pub(super) fn parse_send_keys(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut target = None;
    let mut target_client = None;
    let mut expand_formats = false;
    let mut hex = false;
    let mut literal = false;
    let mut dispatch_key_table = false;
    let mut copy_mode_command = false;
    let mut forward_mouse_event = false;
    let mut reset_terminal = false;
    let mut repeat_count = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        if let Some(cluster) = parse_compact_flag_cluster(&token, "FHlKMRX", "cNt") {
            let _ = args.optional();
            for flag in cluster {
                match flag {
                    CompactFlag::Bare(flag) => match flag {
                        'F' => expand_formats = true,
                        'H' => hex = true,
                        'l' => literal = true,
                        'K' => dispatch_key_table = true,
                        'M' => forward_mouse_event = true,
                        'R' => reset_terminal = true,
                        'X' => copy_mode_command = true,
                        _ => unreachable!("compact send-keys flags are prevalidated"),
                    },
                    compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                        target_client =
                            Some(compact_flag.value_or_next(&mut args, "-c target-client")?);
                    }
                    compact_flag @ CompactFlag::Value { flag: 'N', .. } => {
                        repeat_count = Some(parse_send_keys_repeat_count(
                            &compact_flag.value_or_next(&mut args, "-N count")?,
                        )?);
                    }
                    compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                        target = Some(parse_pane_target(
                            "send-keys",
                            compact_flag.value_or_next(&mut args, "-t target")?,
                        )?);
                    }
                    CompactFlag::Value { flag, .. } => {
                        return Err(unsupported_flag("send-keys", &format!("-{flag}")));
                    }
                };
            }
            continue;
        }
        match token.as_str() {
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
                repeat_count = Some(parse_send_keys_repeat_count(&args.required("-N count")?)?);
            }
            "-p" => return Err(unsupported_flag("send-keys", "-p")),
            value if value.starts_with("-N") && value.len() > 2 => {
                let count = value[2..].to_owned();
                let _ = args.optional();
                repeat_count = Some(parse_send_keys_repeat_count(&count)?);
            }
            "-R" => {
                let _ = args.optional();
                reset_terminal = true;
            }
            "-X" => {
                let _ = args.optional();
                copy_mode_command = true;
            }
            "-c" => {
                let _ = args.optional();
                target_client = Some(args.required("-c target-client")?);
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
        && target_client.is_none()
    {
        return Ok(Request::SendKeys(SendKeysRequest {
            target: target.ok_or_else(|| missing_argument("send-keys", "-t target"))?,
            keys,
        }));
    }

    if target_client.is_some() {
        return Ok(Request::SendKeysExt2(Box::new(
            rmux_proto::SendKeysExt2Request {
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
                target_client,
            },
        )));
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

fn parse_send_keys_repeat_count(value: &str) -> Result<usize, RmuxError> {
    let count = parse_usize("send-keys", "-N", value)?;
    if count == 0 {
        return Err(RmuxError::Message("repeat count too small".to_owned()));
    }
    Ok(count)
}

pub(super) fn parse_bind_key(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut table_name = None;
    let mut note = None;
    let mut repeat = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
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
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "nr", "NT") else {
                    reject_unknown_option_before_positional("bind-key", &token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('n') => table_name = Some("root".to_owned()),
                        CompactFlag::Bare('r') => repeat = true,
                        compact_flag @ CompactFlag::Value { flag: 'N', .. } => {
                            note = Some(compact_flag.value_or_next(&mut args, "-N note")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'T', .. } => {
                            table_name =
                                Some(compact_flag.value_or_next(&mut args, "-T key-table")?);
                        }
                        _ => unreachable!("compact bind-key flags are prevalidated"),
                    }
                }
            }
        }
    }

    let key = args.required("key")?;
    Ok(Request::BindKey(Box::new(rmux_proto::BindKeyRequest {
        table_name: table_name.unwrap_or_else(|| "prefix".to_owned()),
        key,
        note,
        repeat,
        command: (!args.is_empty()).then_some(args.remaining()),
    })))
}

pub(super) fn parse_unbind_key(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut table_name = None;
    let mut all = false;
    let mut quiet = false;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
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
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "anq", "T") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => all = true,
                        CompactFlag::Bare('n') => table_name = Some("root".to_owned()),
                        CompactFlag::Bare('q') => quiet = true,
                        compact_flag @ CompactFlag::Value { flag: 'T', .. } => {
                            table_name =
                                Some(compact_flag.value_or_next(&mut args, "-T key-table")?);
                        }
                        _ => unreachable!("compact unbind-key flags are prevalidated"),
                    }
                }
            }
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

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
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
                sort_order = Some(args.required("-O order")?);
            }
            "-P" => {
                let _ = args.optional();
                prefix = Some(args.required("-P prefix")?);
            }
            "-T" => {
                let _ = args.optional();
                table_name = Some(args.required("-T key-table")?);
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "1aNr", "FOPT") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('1') => first_only = true,
                        CompactFlag::Bare('a') => include_unnoted = true,
                        CompactFlag::Bare('N') => notes = true,
                        CompactFlag::Bare('r') => reversed = true,
                        compact_flag @ CompactFlag::Value { flag: 'F', .. } => {
                            format = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'O', .. } => {
                            sort_order = Some(compact_flag.value_or_next(&mut args, "-O order")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'P', .. } => {
                            prefix = Some(compact_flag.value_or_next(&mut args, "-P prefix")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'T', .. } => {
                            table_name =
                                Some(compact_flag.value_or_next(&mut args, "-T key-table")?);
                        }
                        _ => unreachable!("compact list-keys flags are prevalidated"),
                    }
                }
            }
        }
    }

    let key = args.optional();
    args.no_extra("list-keys")?;
    Ok(Request::ListKeys(Box::new(rmux_proto::ListKeysRequest {
        table_name,
        first_only,
        notes,
        include_unnoted,
        reversed,
        format,
        sort_order,
        prefix,
        key,
    })))
}

pub(super) fn parse_send_prefix(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut secondary = false;
    let mut target = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
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
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "2", "t") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('2') => secondary = true,
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_pane_target(
                                "send-prefix",
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        _ => unreachable!("compact send-prefix flags are prevalidated"),
                    }
                }
            }
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

    #[test]
    fn parse_send_keys_accepts_tmux_compact_copy_mode_target() {
        let request = parse_send_keys(CommandTokens::new(vec![
            token("-Xt="),
            token("select-word"),
        ]))
        .expect("compact copy-mode target parses");

        let Request::SendKeysExt(request) = request else {
            panic!("compact copy-mode target must use extended send-keys request");
        };
        assert!(request.copy_mode_command);
        assert_eq!(
            request.target,
            Some(parse_pane_target("send-keys", "=".to_owned()).unwrap())
        );
        assert_eq!(request.keys, vec!["select-word"]);
    }

    #[test]
    fn parse_send_keys_accepts_mixed_bare_and_value_flag_clusters() {
        let request = parse_send_keys(CommandTokens::new(vec![token("-lN2"), token("literal")]))
            .expect("mixed send-keys cluster parses");

        let Request::SendKeysExt(request) = request else {
            panic!("mixed cluster must use extended send-keys request");
        };
        assert!(request.literal);
        assert_eq!(request.repeat_count, Some(2));
        assert_eq!(request.keys, vec!["literal"]);
    }

    #[test]
    fn parse_bind_key_accepts_clusters_attached_values_and_separator() {
        let request = parse_bind_key(CommandTokens::new(vec![
            token("-nrTroot"),
            token("-N"),
            token("cluster note"),
            token("C-a"),
            token("display-message"),
            token("ok"),
        ]))
        .expect("clustered bind-key flags parse");

        let Request::BindKey(request) = request else {
            panic!("expected bind-key request");
        };
        assert_eq!(request.table_name, "root");
        assert!(request.repeat);
        assert_eq!(request.note.as_deref(), Some("cluster note"));
        assert_eq!(request.key, "C-a");

        let separated = parse_bind_key(CommandTokens::new(vec![
            token("--"),
            token("-nr"),
            token("display-message"),
            token("literal-key"),
        ]))
        .expect("bind-key separator preserves a flag-looking key");
        let Request::BindKey(separated) = separated else {
            panic!("expected separated bind-key request");
        };
        assert_eq!(separated.table_name, "prefix");
        assert!(!separated.repeat);
        assert_eq!(separated.key, "-nr");
    }

    #[test]
    fn parse_unbind_key_accepts_bare_clusters_and_attached_table() {
        let request = parse_unbind_key(CommandTokens::new(vec![token("-aqTroot")]))
            .expect("clustered unbind-key flags parse");

        let Request::UnbindKey(request) = request else {
            panic!("expected unbind-key request");
        };
        assert!(request.all);
        assert!(request.quiet);
        assert_eq!(request.table_name, "root");
        assert!(request.key.is_none());
    }

    #[test]
    fn parse_list_keys_accepts_bare_cluster_before_attached_value_flags() {
        let request = parse_list_keys(CommandTokens::new(vec![
            token("-1aNrF#{key}"),
            token("-Okey"),
            token("-Pprefix"),
            token("-Troot"),
        ]))
        .expect("clustered list-keys flags parse");

        let Request::ListKeys(request) = request else {
            panic!("expected list-keys request");
        };
        assert!(request.first_only);
        assert!(request.include_unnoted);
        assert!(request.notes);
        assert!(request.reversed);
        assert_eq!(request.format.as_deref(), Some("#{key}"));
        assert_eq!(request.sort_order.as_deref(), Some("key"));
        assert_eq!(request.prefix.as_deref(), Some("prefix"));
        assert_eq!(request.table_name.as_deref(), Some("root"));
    }

    #[test]
    fn parse_send_prefix_accepts_bare_and_attached_target_cluster() {
        let request = parse_send_prefix(CommandTokens::new(vec![token("-2talpha:0.0")]))
            .expect("clustered send-prefix flags parse");

        let Request::SendPrefix(request) = request else {
            panic!("expected send-prefix request");
        };
        assert!(request.secondary);
        assert_eq!(
            request.target,
            Some(parse_pane_target("send-prefix", "alpha:0.0".to_owned()).unwrap())
        );
    }

    #[test]
    fn parse_send_keys_rejects_zero_repeat_count() {
        let error = parse_send_keys(CommandTokens::new(vec![token("-N0"), token("A")]))
            .expect_err("send-keys -N0 must reject zero repeats");

        assert_eq!(error.to_string(), "repeat count too small");
    }

    #[test]
    fn parse_send_keys_rejects_unknown_prefix_flag() {
        let error = parse_send_keys(CommandTokens::new(vec![token("-p"), token("abc")]))
            .expect_err("send-keys -p should be rejected before keys");

        assert_eq!(
            error,
            RmuxError::Server("command send-keys: unknown flag -p".to_owned())
        );
    }
}
