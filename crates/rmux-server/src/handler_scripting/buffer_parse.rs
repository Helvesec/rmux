use std::path::{Path, PathBuf};

use rmux_core::{SessionStore, TargetFindContext};
use rmux_proto::{DeleteBufferRequest, ListBuffersRequest, LoadBufferRequest, Request, RmuxError};

use super::tokens::CommandTokens;
use super::values::{missing_argument, unsupported_flag};
use super::{implicit_pane_target, parse_pane_target};

pub(super) fn parse_set_buffer(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut name = None;
    let mut append = false;
    let mut new_name = None;
    let mut set_clipboard = false;
    while let Some(token) = args.peek() {
        match token {
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
                let _ = args.required("-t target")?;
            }
            "-w" => {
                let _ = args.optional();
                set_clipboard = true;
            }
            _ => break,
        }
    }
    let content_parts = args.remaining();
    if new_name.is_none() && content_parts.is_empty() {
        return Err(missing_argument("set-buffer", "content"));
    }
    let content = content_parts.join(" ");

    Ok(Request::SetBuffer(rmux_proto::SetBufferRequest {
        name,
        content: content.into_bytes(),
        append,
        new_name,
        set_clipboard,
    }))
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
    let sort_order = None;
    let reversed = false;

    while let Some(token) = args.optional() {
        match token.as_str() {
            "-F" => format = Some(args.required("-F format")?),
            "-f" => filter = Some(args.required("-f filter")?),
            "-O" => return Err(unsupported_flag("list-buffers", "-O")),
            "-r" => return Err(unsupported_flag("list-buffers", "-r")),
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
            flag if flag.starts_with('-') => return Err(unsupported_flag("load-buffer", flag)),
            _ => break,
        }
    }
    let path = args.required("load-buffer path")?;
    args.no_extra("load-buffer")?;
    Ok(Request::LoadBuffer(LoadBufferRequest {
        path,
        cwd: caller_cwd.map(PathBuf::from),
        name,
        set_clipboard,
    }))
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
