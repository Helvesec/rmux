use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use rmux_core::{
    command_parser::{CommandArgument, ParsedCommand},
    SessionStore, TargetFindContext, TargetFindFlags, TargetFindType, UnresolvedTarget,
};
use rmux_proto::{RmuxError, SessionName, Target, WindowTarget};

use super::command_args::{
    command_argument_for_error, pop_command_list_argument, pop_string_argument, CommandListArgument,
};
use super::source_files::ParsedSourceFileCommand;
use super::source_files::SourceSyntax;
use super::targets::{
    implicit_session_name, implicit_window_target, parse_pane_target,
    parse_queued_new_window_target_argument, parse_target_arg, resolve_queue_target_argument,
    NewWindowTargetIndex,
};
use super::tokens::{parse_compact_flag_cluster, CompactFlag};
use super::values::{missing_argument, unsupported_flag};
use crate::pane_terminals::session_not_found;

#[derive(Debug, Clone)]
pub(super) struct ParsedNewWindowCommand {
    pub(super) target: SessionName,
    pub(super) target_window_index: Option<NewWindowTargetIndex>,
    pub(super) insert_at_target: bool,
    pub(super) name: Option<String>,
    pub(super) detached: bool,
    pub(super) start_directory: Option<PathBuf>,
    pub(super) environment: Option<Vec<String>>,
    pub(super) command: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
pub(super) struct ParsedIfShellCommand {
    pub(super) condition: String,
    pub(super) background: bool,
    pub(super) format_mode: bool,
    pub(super) then_commands: CommandListArgument,
    pub(super) else_commands: Option<CommandListArgument>,
    pub(super) target: Option<Target>,
    pub(super) caller_cwd: Option<PathBuf>,
}

pub(super) fn parse_queued_new_window(
    command: ParsedCommand,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedNewWindowCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
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

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        match token {
            "--" => {
                let _ = args.pop_front();
                command_only = true;
                break;
            }
            "-c" => {
                let _ = args.pop_front();
                start_directory = Some(PathBuf::from(pop_string_argument(
                    &mut args,
                    "-c start-directory",
                )?));
            }
            "-a" => {
                let _ = args.pop_front();
                after = true;
                before = false;
            }
            "-b" => {
                let _ = args.pop_front();
                after = false;
                before = true;
            }
            "-e" => {
                let _ = args.pop_front();
                environment.push(pop_string_argument(&mut args, "-e name=value")?);
            }
            "-t" => {
                let _ = args.pop_front();
                let raw_target = pop_string_argument(&mut args, "-t target")?;
                target_has_signed_window_part =
                    signed_window_target_session_part(&raw_target).is_some();
                let (session_name, window_index) =
                    parse_queued_new_window_target_argument(raw_target, sessions, find_context)?;
                target = Some(session_name);
                target_window_index = window_index;
            }
            "-n" => {
                let _ = args.pop_front();
                name = Some(pop_string_argument(&mut args, "-n name")?);
            }
            "-d" => {
                let _ = args.pop_front();
                detached = true;
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "abd", "cetn") else {
                    break;
                };
                let _ = args.pop_front();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('a') => {
                            after = true;
                            before = false;
                        }
                        CompactFlag::Bare('b') => {
                            after = false;
                            before = true;
                        }
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("new-window", &format!("-{flag}")))
                        }
                        CompactFlag::Value { flag: 'c', value } => {
                            start_directory = Some(PathBuf::from(compact_value_or_next_argument(
                                &mut args,
                                value,
                                "-c start-directory",
                            )?));
                        }
                        CompactFlag::Value { flag: 'e', value } => {
                            environment.push(compact_value_or_next_argument(
                                &mut args,
                                value,
                                "-e name=value",
                            )?);
                        }
                        CompactFlag::Value { flag: 't', value } => {
                            let raw_target =
                                compact_value_or_next_argument(&mut args, value, "-t target")?;
                            target_has_signed_window_part =
                                signed_window_target_session_part(&raw_target).is_some();
                            let (session_name, window_index) =
                                parse_queued_new_window_target_argument(
                                    raw_target,
                                    sessions,
                                    find_context,
                                )?;
                            target = Some(session_name);
                            target_window_index = window_index;
                        }
                        CompactFlag::Value { flag: 'n', value } => {
                            name =
                                Some(compact_value_or_next_argument(&mut args, value, "-n name")?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("new-window", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }

    if !command_only
        && args
            .front()
            .and_then(CommandArgument::as_string)
            .is_some_and(|token| token.starts_with('-'))
    {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for new-window",
            command_argument_for_error(args.front().expect("peeked argument must exist"))
        )));
    }

    let command = (!args.is_empty()).then_some(
        args.into_iter()
            .map(|argument| match argument {
                CommandArgument::String(value) => Ok(value),
                CommandArgument::Commands(_) => Err(RmuxError::Server(
                    "new-window command must be a string argument".to_owned(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?,
    );

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
            target_window_index =
                Some(NewWindowTargetIndex::Absolute(window_target.window_index()));
        }
        if after {
            target_window_index = Some(
                target_window_index
                    .expect("placement target index must exist")
                    .checked_add_one()?,
            );
        }
    }

    Ok(ParsedNewWindowCommand {
        target: target.unwrap_or(implicit_session_name(sessions, find_context, "new-window")?),
        target_window_index,
        insert_at_target,
        name,
        detached,
        start_directory,
        environment: (!environment.is_empty()).then_some(environment),
        command,
    })
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

fn compact_value_or_next_argument(
    args: &mut VecDeque<CommandArgument>,
    value: Option<String>,
    description: &str,
) -> Result<String, RmuxError> {
    match value {
        Some(value) => Ok(value),
        None => pop_string_argument(args, description),
    }
}

fn signed_window_index_target(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(['+', '-']) else {
        return false;
    };
    rest.is_empty() || rest.bytes().all(|byte| byte.is_ascii_digit())
}

fn parse_queued_if_shell_target(
    raw_target: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Target, RmuxError> {
    let value = resolve_queue_target_argument("if-shell", 't', raw_target, sessions, find_context)?;
    parse_target_arg("if-shell", value)
}

fn parse_queued_if_shell_attached_target(
    raw_target: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<Target, RmuxError> {
    if matches!(raw_target.as_str(), "=" | "{mouse}") && find_context.mouse_target().is_none() {
        return sessions.resolve_unresolved_target(
            &UnresolvedTarget::none(),
            TargetFindType::Pane,
            TargetFindFlags::CANFAIL,
            find_context,
        );
    }
    parse_queued_if_shell_target(raw_target, sessions, find_context)
}

pub(super) fn parse_queued_if_shell(
    command: ParsedCommand,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedIfShellCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut background = false;
    let mut format_mode = false;
    let mut target = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        match token {
            "--" => {
                let _ = args.pop_front();
                break;
            }
            "-F" => {
                let _ = args.pop_front();
                format_mode = true;
            }
            "-b" => {
                let _ = args.pop_front();
                background = true;
            }
            "-t" => {
                let _ = args.pop_front();
                target = Some(parse_queued_if_shell_attached_target(
                    pop_string_argument(&mut args, "-t target")?,
                    sessions,
                    find_context,
                )?);
            }
            token => {
                let Some(cluster) = parse_compact_flag_cluster(token, "bF", "t") else {
                    break;
                };
                let _ = args.pop_front();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('b') => background = true,
                        CompactFlag::Bare('F') => format_mode = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("if-shell", &format!("-{flag}")));
                        }
                        CompactFlag::Value { flag: 't', value } => {
                            let value =
                                compact_value_or_next_argument(&mut args, value, "-t target")?;
                            target = Some(parse_queued_if_shell_attached_target(
                                value,
                                sessions,
                                find_context,
                            )?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("if-shell", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }

    let condition = pop_string_argument(&mut args, "if-shell condition")?;
    let then_commands = pop_command_list_argument(&mut args, "if-shell then command")?;
    let else_commands = if args.is_empty() {
        None
    } else {
        Some(pop_command_list_argument(
            &mut args,
            "if-shell else command",
        )?)
    };
    if let Some(extra) = args.front() {
        return Err(RmuxError::Server(format!(
            "unexpected argument '{}' for if-shell",
            command_argument_for_error(extra)
        )));
    }

    Ok(ParsedIfShellCommand {
        condition,
        background,
        format_mode,
        then_commands,
        else_commands,
        target,
        caller_cwd: caller_cwd.map(Path::to_path_buf),
    })
}

pub(super) fn parse_queued_source_file(
    command: ParsedCommand,
    caller_cwd: Option<&Path>,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedSourceFileCommand, RmuxError> {
    let mut args = VecDeque::from(command.arguments().to_vec());
    let mut quiet = false;
    let mut parse_only = false;
    let mut verbose = false;
    let mut expand_paths = false;
    let mut target = None;

    while let Some(token) = args.front().and_then(CommandArgument::as_string) {
        match token {
            "--" => {
                let _ = args.pop_front();
                break;
            }
            "-" => break,
            "-F" => {
                let _ = args.pop_front();
                expand_paths = true;
            }
            "-n" => {
                let _ = args.pop_front();
                parse_only = true;
            }
            "-q" => {
                let _ = args.pop_front();
                quiet = true;
            }
            "-v" => {
                let _ = args.pop_front();
                verbose = true;
            }
            "-t" => {
                let _ = args.pop_front();
                target = Some(parse_queued_source_file_target(
                    pop_string_argument(&mut args, "-t target")?,
                    sessions,
                    find_context,
                )?);
            }
            token if token.starts_with('-') => {
                let Some(cluster) = parse_compact_flag_cluster(token, "Fnqv", "t") else {
                    return Err(unsupported_flag("source-file", token));
                };
                let _ = args.pop_front();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('F') => expand_paths = true,
                        CompactFlag::Bare('n') => parse_only = true,
                        CompactFlag::Bare('q') => quiet = true,
                        CompactFlag::Bare('v') => verbose = true,
                        CompactFlag::Bare(flag) => {
                            return Err(unsupported_flag("source-file", &format!("-{flag}")));
                        }
                        CompactFlag::Value { flag: 't', value } => {
                            let value =
                                compact_value_or_next_argument(&mut args, value, "-t target")?;
                            target = Some(parse_queued_source_file_target(
                                value,
                                sessions,
                                find_context,
                            )?);
                        }
                        CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("source-file", &format!("-{flag}")));
                        }
                    }
                }
            }
            _ => break,
        }
    }

    let mut paths = Vec::new();
    while let Some(argument) = args.pop_front() {
        paths.push(match argument {
            CommandArgument::String(value) => value,
            CommandArgument::Commands(_) => {
                return Err(RmuxError::Server(
                    "source-file path must be a string argument".to_owned(),
                ));
            }
        });
    }
    if paths.is_empty() {
        return Err(missing_argument("source-file", "path"));
    }

    Ok(ParsedSourceFileCommand {
        paths,
        quiet,
        parse_only,
        verbose,
        expand_paths,
        target,
        caller_cwd: caller_cwd.map(Path::to_path_buf),
        stdin: None,
        current_file: None,
        syntax: SourceSyntax::Rmux,
    })
}

fn parse_queued_source_file_target(
    value: String,
    sessions: &SessionStore,
    find_context: &TargetFindContext,
) -> Result<rmux_proto::PaneTarget, RmuxError> {
    parse_pane_target("source-file", value.clone()).or_else(|_| {
        let resolved =
            resolve_queue_target_argument("source-file", 't', value, sessions, find_context)?;
        parse_pane_target("source-file", resolved)
    })
}
