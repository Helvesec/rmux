//! `set-environment` and `show-environment` parsing and their global-or-session scope.

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::request::Request;
use rmux_proto::{
    RmuxError, ScopeSelector, SessionName, SetEnvironmentMode, SetEnvironmentRequest,
    ShowEnvironmentRequest,
};

use super::super::parse_session_target;
use super::super::targets::implicit_session_name;
use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::{reject_unknown_option_before_positional, unsupported_flag};

pub(in crate::handler::scripting_support) fn parse_set_environment(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut format = false;
    let mut hidden = false;
    let mut mode = SetEnvironmentMode::Set;
    let mut target = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-F" => {
                let _ = args.optional();
                format = true;
            }
            "-g" => {
                let _ = args.optional();
                global = true;
            }
            "-h" => {
                let _ = args.optional();
                hidden = true;
            }
            "-r" => {
                let _ = args.optional();
                select_set_environment_mode(&mut mode, SetEnvironmentMode::Clear);
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_session_target(args.required("-t target")?)?);
            }
            "-u" => {
                let _ = args.optional();
                select_set_environment_mode(&mut mode, SetEnvironmentMode::Unset);
            }
            _ => {
                let Some(cluster) =
                    parse_compact_flag_cluster("set-environment", &token, "Fghru", "t")?
                else {
                    reject_unknown_option_before_positional("set-environment", &token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('F') => format = true,
                        CompactFlag::Bare('g') => global = true,
                        CompactFlag::Bare('h') => hidden = true,
                        CompactFlag::Bare('r') => {
                            select_set_environment_mode(&mut mode, SetEnvironmentMode::Clear);
                        }
                        CompactFlag::Bare('u') => {
                            select_set_environment_mode(&mut mode, SetEnvironmentMode::Unset);
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_session_target(
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        _ => unreachable!("compact set-environment flags are prevalidated"),
                    }
                }
            }
        }
    }

    let scope =
        build_global_or_session_scope("set-environment", global, target, sessions, find_context)?;
    let name = args.required("set-environment name")?;
    let value = match mode {
        SetEnvironmentMode::Set => args
            .optional()
            .ok_or_else(|| RmuxError::Server("no value specified".to_owned()))?,
        SetEnvironmentMode::Clear | SetEnvironmentMode::Unset => {
            args.optional().unwrap_or_default()
        }
    };
    args.no_extra("set-environment")?;

    Ok(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
        scope,
        name,
        value,
        mode: Some(mode),
        hidden,
        format,
    })))
}

fn select_set_environment_mode(current: &mut SetEnvironmentMode, requested: SetEnvironmentMode) {
    if matches!(requested, SetEnvironmentMode::Unset)
        || !matches!(*current, SetEnvironmentMode::Unset)
    {
        *current = requested;
    }
}

pub(in crate::handler::scripting_support) fn parse_show_environment(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let mut global = false;
    let mut hidden = false;
    let mut shell_format = false;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-g" => {
                let _ = args.optional();
                global = true;
            }
            "-h" => {
                let _ = args.optional();
                hidden = true;
            }
            "-s" => {
                let _ = args.optional();
                shell_format = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_session_target(args.required("-t target")?)?);
            }
            flag if flag.starts_with('-') => {
                return Err(unsupported_flag("show-environment", flag));
            }
            _ => break,
        }
    }

    let scope =
        build_global_or_session_scope("show-environment", global, target, sessions, find_context)?;
    let name = args.optional();
    args.no_extra("show-environment")?;

    Ok(Request::ShowEnvironment(ShowEnvironmentRequest {
        scope,
        name,
        hidden,
        shell_format,
    }))
}

fn build_global_or_session_scope(
    command: &str,
    global: bool,
    target: Option<SessionName>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ScopeSelector, RmuxError> {
    match (global, target) {
        (true, None) => Ok(ScopeSelector::Global),
        (false, Some(session_name)) => Ok(ScopeSelector::Session(session_name)),
        (false, None) => Ok(ScopeSelector::Session(implicit_session_name(
            sessions,
            find_context,
            command,
        )?)),
        _ => Err(RmuxError::Server(format!(
            "{command} accepts at most one of -g or -t target"
        ))),
    }
}
