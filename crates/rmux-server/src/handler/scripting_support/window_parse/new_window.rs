//! `new-window` argument parsing, including its signed window-index target forms.

use std::path::PathBuf;

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{NewWindowRequest, Request, RmuxError, WindowTarget};

use crate::pane_terminals::session_not_found;

use super::super::targets::signed_window_target_session_part;
use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::unsupported_flag;
use super::super::{
    implicit_session_name, implicit_window_target, parse_new_window_target_argument,
};

pub(in crate::handler::scripting_support) fn parse_new_window(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut environment = Vec::new();
    let mut target = None;
    let mut target_window_index = None;
    let mut name = None;
    let mut detached = false;
    let mut after = false;
    let mut before = false;
    let mut target_has_signed_window_part = false;
    let mut start_directory = None;
    let mut command_only = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                command_only = true;
                break;
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(PathBuf::from(args.required("-c start-directory")?));
            }
            "-a" => {
                let _ = args.optional();
                after = true;
            }
            "-b" => {
                let _ = args.optional();
                before = true;
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e name=value")?);
            }
            "-t" => {
                let _ = args.optional();
                let raw_target = args.required("-t target")?;
                target_has_signed_window_part =
                    signed_window_target_session_part(&raw_target).is_some();
                let (session_name, window_index) =
                    parse_new_window_target_argument(raw_target, sessions, find_context)?;
                target = Some(session_name);
                target_window_index = window_index;
            }
            "-n" => {
                let _ = args.optional();
                name = Some(args.required("-n name")?);
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster("new-window", token, "abd", "cetn")?
                else {
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => after = true,
                        CompactFlag::Bare('b') => before = true,
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("new-window", &format!("-{flag}")));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                            start_directory = Some(PathBuf::from(
                                compact_flag.value_or_next(&mut args, "-c start-directory")?,
                            ));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'e', .. } => {
                            environment
                                .push(compact_flag.value_or_next(&mut args, "-e name=value")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            let raw_target = compact_flag.value_or_next(&mut args, "-t target")?;
                            target_has_signed_window_part =
                                signed_window_target_session_part(&raw_target).is_some();
                            let (session_name, window_index) = parse_new_window_target_argument(
                                raw_target,
                                sessions,
                                find_context,
                            )?;
                            target = Some(session_name);
                            target_window_index = window_index;
                        }
                        compact_flag @ CompactFlag::Value { flag: 'n', .. } => {
                            name = Some(compact_flag.value_or_next(&mut args, "-n name")?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("new-window", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }

    if !command_only && args.peek().is_some_and(|token| token.starts_with('-')) {
        args.no_extra("new-window")?;
    }

    let command = {
        let remaining = args.remaining();
        (!remaining.is_empty()).then_some(remaining)
    };

    let insert_at_target = after || before;
    if insert_at_target {
        if target_window_index.is_none() || target_has_signed_window_part {
            let window_target = if let Some(session_name) = target.as_ref() {
                let window_index = sessions
                    .session(session_name)
                    .ok_or_else(|| session_not_found(session_name))?
                    .active_window_index();
                WindowTarget::with_window(session_name.clone(), window_index)
            } else {
                implicit_window_target(sessions, find_context, "new-window")?
            };
            target = Some(window_target.session_name().clone());
            target_window_index = Some(window_target.window_index());
        }
        if after {
            target_window_index = Some(
                target_window_index
                    .expect("placement target index must exist")
                    .checked_add(1)
                    .ok_or_else(|| {
                        RmuxError::Server("window index space exhausted for new-window".to_owned())
                    })?,
            );
        }
    }

    Ok(Request::NewWindow(Box::new(NewWindowRequest {
        target: target.unwrap_or(implicit_session_name(sessions, find_context, "new-window")?),
        name,
        detached,
        start_directory,
        environment: (!environment.is_empty()).then_some(environment),
        command,
        process_command: None,
        target_window_index,
        insert_at_target,
    })))
}
