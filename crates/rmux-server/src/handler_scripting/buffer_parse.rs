use std::path::{Path, PathBuf};

use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{DeleteBufferRequest, ListBuffersRequest, LoadBufferRequest, Request, RmuxError};

use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{missing_argument, unsupported_flag};
use super::{implicit_pane_target, parse_pane_target};

pub(super) fn parse_set_buffer(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut name = None;
    let mut append = false;
    let mut new_name = None;
    let mut set_clipboard = false;
    let mut target_client = None;
    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-a" => {
                let _ = args.optional();
                append = true;
            }
            "-b" => {
                let _ = args.optional();
                name = Some(args.required("-b buffer name")?);
            }
            "-n" => {
                let _ = args.optional();
                new_name = Some(args.required("-n buffer name")?);
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            "-w" => {
                let _ = args.optional();
                set_clipboard = true;
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(&token, "aw", "bnt") else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => append = true,
                        CompactFlag::Bare('w') => set_clipboard = true,
                        compact_flag @ CompactFlag::Value { flag: 'b', .. } => {
                            name = Some(compact_flag.value_or_next(&mut args, "-b buffer name")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'n', .. } => {
                            new_name =
                                Some(compact_flag.value_or_next(&mut args, "-n buffer name")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target_client =
                                Some(compact_flag.value_or_next(&mut args, "-t target-client")?);
                        }
                        _ => unreachable!("compact set-buffer flags are prevalidated"),
                    }
                }
            }
        }
    }
    let content_parts = args.remaining();
    if new_name.is_none() && content_parts.is_empty() {
        return Err(missing_argument("set-buffer", "content"));
    }
    let content = content_parts.join(" ");

    Ok(Request::SetBuffer(Box::new(rmux_proto::SetBufferRequest {
        name,
        content: content.into_bytes(),
        append,
        new_name,
        set_clipboard,
        target_client,
    })))
}

pub(super) fn parse_show_buffer(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let name = parse_optional_buffer_name("show-buffer", &mut args)?;
    args.no_extra("show-buffer")?;
    Ok(Request::ShowBuffer(rmux_proto::ShowBufferRequest { name }))
}

pub(super) fn parse_paste_buffer(
    mut args: CommandTokens,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut name = None;
    let mut target = None;
    let mut delete_after = false;
    let mut separator = None;
    let mut linefeed = false;
    let mut raw = false;
    let mut bracketed = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-b" => name = Some(args.required("-b buffer name")?),
            "-t" => {
                target = Some(parse_pane_target(
                    "paste-buffer",
                    args.required("-t target")?,
                )?)
            }
            "-d" => delete_after = true,
            "-p" => bracketed = true,
            "-r" => linefeed = true,
            "-S" => raw = true,
            "-s" => separator = Some(args.required("-s separator")?),
            flag if flag.starts_with('-') => return Err(unsupported_flag("paste-buffer", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for paste-buffer"
                )));
            }
        }
    }

    Ok(Request::PasteBuffer(Box::new(
        rmux_proto::PasteBufferRequest {
            name,
            target: target.unwrap_or(implicit_pane_target(
                sessions,
                find_context,
                "paste-buffer",
            )?),
            delete_after,
            separator,
            linefeed,
            raw,
            bracketed,
        },
    )))
}

pub(super) fn parse_list_buffers(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut format = None;
    let mut filter = None;
    let mut sort_order = None;
    let mut reversed = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-F" => format = Some(args.required("-F format")?),
            "-f" => filter = Some(args.required("-f filter")?),
            "-O" => sort_order = Some(args.required("-O order")?),
            "-r" => reversed = true,
            flag if flag.starts_with('-') => return Err(unsupported_flag("list-buffers", flag)),
            _ => {
                return Err(RmuxError::Server(format!(
                    "unexpected argument '{token}' for list-buffers"
                )));
            }
        }
    }

    Ok(Request::ListBuffers(ListBuffersRequest {
        format,
        filter,
        sort_order,
        reversed,
    }))
}

pub(super) fn parse_delete_buffer(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let name = parse_optional_buffer_name("delete-buffer", &mut args)?;
    args.no_extra("delete-buffer")?;
    Ok(Request::DeleteBuffer(DeleteBufferRequest { name }))
}

pub(super) fn parse_load_buffer(
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
) -> Result<Request, RmuxError> {
    let mut name = None;
    let mut set_clipboard = false;
    let mut target_client = None;
    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-b" => {
                let _ = args.optional();
                name = Some(args.required("-b buffer name")?);
            }
            "-w" => {
                let _ = args.optional();
                set_clipboard = true;
            }
            "-t" => {
                let _ = args.optional();
                target_client = Some(args.required("-t target-client")?);
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("load-buffer", flag)),
            _ => break,
        }
    }
    let path = args.required("load-buffer path")?;
    args.no_extra("load-buffer")?;
    Ok(Request::LoadBuffer(Box::new(LoadBufferRequest {
        path,
        cwd: caller_cwd.map(PathBuf::from),
        name,
        set_clipboard,
        target_client,
    })))
}

pub(super) fn parse_save_buffer(
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
) -> Result<Request, RmuxError> {
    let mut name = None;
    let mut append = false;
    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-b" => {
                let _ = args.optional();
                name = Some(args.required("-b buffer name")?);
            }
            "-a" => {
                let _ = args.optional();
                append = true;
            }
            flag if flag.starts_with('-') => return Err(unsupported_flag("save-buffer", flag)),
            _ => break,
        }
    }
    let path = args.required("save-buffer path")?;
    args.no_extra("save-buffer")?;
    Ok(Request::SaveBuffer(rmux_proto::SaveBufferRequest {
        path,
        cwd: caller_cwd.map(PathBuf::from),
        name,
        append,
    }))
}

fn parse_optional_buffer_name(
    command: &str,
    args: &mut CommandTokens,
) -> Result<Option<String>, RmuxError> {
    let mut name = None;
    while args.peek_is_flag() {
        match args.required("buffer flag")?.as_str() {
            "-b" => name = Some(args.required("-b buffer name")?),
            flag => return Err(unsupported_flag(command, flag)),
        }
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(value: &str) -> String {
        value.to_owned()
    }

    #[test]
    fn parse_set_buffer_accepts_bare_cluster_before_value_flags() {
        let request = parse_set_buffer(CommandTokens::new(vec![
            token("-aw"),
            token("-baudit"),
            token("tail"),
        ]))
        .expect("clustered set-buffer flags parse");

        let Request::SetBuffer(request) = request else {
            panic!("expected set-buffer request");
        };
        assert!(request.append);
        assert!(request.set_clipboard);
        assert_eq!(request.name.as_deref(), Some("audit"));
        assert_eq!(request.content, b"tail");
    }

    #[test]
    fn parse_set_buffer_stops_parsing_flags_at_separator() {
        let request = parse_set_buffer(CommandTokens::new(vec![token("--"), token("-aw")]))
            .expect("separator preserves flag-looking buffer content");

        let Request::SetBuffer(request) = request else {
            panic!("expected set-buffer request");
        };
        assert!(!request.append);
        assert!(!request.set_clipboard);
        assert_eq!(request.content, b"-aw");
    }
}
