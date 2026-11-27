//! `server-access` parsing: access-control flags, separate from the client command parsers.

use rmux_proto::{Request, RmuxError};

use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::unsupported_flag;

pub(in crate::handler::scripting_support) fn parse_server_access(
    mut args: CommandTokens,
) -> Result<Request, RmuxError> {
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
                let Some(cluster) =
                    parse_compact_flag_cluster("server-access", &token, "adlrw", "t")?
                else {
                    match token.as_str() {
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
