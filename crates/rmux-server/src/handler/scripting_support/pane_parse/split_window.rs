//! `split-window` parsing, including the queued form that can print the new pane.

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{Request, RmuxError, SplitDirection, SplitWindowExtRequest, SplitWindowRequest};

use super::super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::super::values::{
    parse_percentage, reject_unknown_option_before_positional, unsupported_flag,
};
use super::super::{implicit_split_target, parse_split_window_target};

const DEFAULT_SPLIT_WINDOW_PRINT_FORMAT: &str = "#{session_name}:#{window_index}.#{pane_index}";

#[derive(Debug, Clone)]
pub(in crate::handler::scripting_support) struct ParsedSplitWindowCommand {
    pub(in crate::handler::scripting_support) request: Request,
    pub(in crate::handler::scripting_support) exit_policy:
        Option<crate::pane_terminals::SplitExitPolicyOverride>,
    pub(in crate::handler::scripting_support) print_target: bool,
    pub(in crate::handler::scripting_support) format: String,
}

impl ParsedSplitWindowCommand {
    pub(in crate::handler::scripting_support) fn install_stdin_payload(
        &mut self,
        payload: Vec<u8>,
    ) -> Result<(), RmuxError> {
        match &mut self.request {
            Request::SplitWindowExt(request)
                if request.stdin_payload.is_some() && request.command.is_none() =>
            {
                request.stdin_payload = Some(payload);
                Ok(())
            }
            _ => Err(RmuxError::Server(
                "split input requires -I without a command".to_owned(),
            )),
        }
    }
}

pub(in crate::handler::scripting_support) fn parse_split_window(
    args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    parse_split_window_command(args, sessions, find_context, false).map(|command| command.request)
}

pub(in crate::handler::scripting_support) fn parse_queued_split_window(
    args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedSplitWindowCommand, RmuxError> {
    parse_split_window_command(args, sessions, find_context, true)
}

fn parse_split_window_command(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    allow_print_flags: bool,
) -> Result<ParsedSplitWindowCommand, RmuxError> {
    let mut direction = SplitDirection::Vertical;
    let mut direction_set = false;
    let mut before = false;
    let mut environment = Vec::new();
    let mut target = None;
    let mut start_directory: Option<std::path::PathBuf> = None;
    let mut detached = false;
    let mut size = None;
    let mut percentage_size = None;
    let mut full_size = false;
    let mut preserve_zoom = false;
    let mut print_target = false;
    let mut format = None;
    let mut stdin = false;
    let mut keep_alive_on_exit = false;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-h" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "split-window accepts only one of -h or -v".to_owned(),
                    ));
                }
                direction = SplitDirection::Horizontal;
                direction_set = true;
            }
            "-v" => {
                let _ = args.optional();
                if direction_set {
                    return Err(RmuxError::Server(
                        "split-window accepts only one of -h or -v".to_owned(),
                    ));
                }
                direction = SplitDirection::Vertical;
                direction_set = true;
            }
            "-b" => {
                let _ = args.optional();
                before = true;
            }
            "-d" => {
                let _ = args.optional();
                detached = true;
            }
            "-f" => {
                let _ = args.optional();
                full_size = true;
            }
            "-Z" => {
                let _ = args.optional();
                preserve_zoom = true;
            }
            "-l" => {
                let _ = args.optional();
                size = Some(args.required("-l size")?);
            }
            "-p" => {
                let _ = args.optional();
                let percentage = args.required("-p size")?;
                if size.is_none() {
                    percentage_size = Some(percentage);
                }
            }
            token if legacy_percentage_attached_value(token).is_some() => {
                let percentage = legacy_percentage_attached_value(token)
                    .expect("checked above")
                    .to_owned();
                let _ = args.optional();
                if size.is_none() {
                    percentage_size = Some(percentage);
                }
            }
            "-P" if allow_print_flags => {
                let _ = args.optional();
                print_target = true;
            }
            "-F" if allow_print_flags => {
                let _ = args.optional();
                format = Some(args.required("-F format")?);
            }
            "-I" => {
                let _ = args.optional();
                stdin = true;
            }
            "-k" => {
                let _ = args.optional();
                keep_alive_on_exit = true;
            }
            "-F" | "-P" => {
                return Err(unsupported_flag("split-window", token));
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(std::path::PathBuf::from(
                    args.required("-c start-directory")?,
                ));
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_split_window_target(args.required("-t target")?)?);
            }
            "-e" => {
                let _ = args.optional();
                environment.push(args.required("-e name=value")?);
            }
            token => {
                let Some(cluster) =
                    parse_compact_flag_cluster("split-window", token, "bdfhIkpvZP", "ceFltp")?
                else {
                    reject_unknown_option_before_positional("split-window", token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        CompactFlag::Bare('b') => before = true,
                        CompactFlag::Bare('d') => detached = true,
                        CompactFlag::Bare('f') => full_size = true,
                        CompactFlag::Bare('h') => {
                            if direction_set {
                                return Err(RmuxError::Server(
                                    "split-window accepts only one of -h or -v".to_owned(),
                                ));
                            }
                            direction = SplitDirection::Horizontal;
                            direction_set = true;
                        }
                        CompactFlag::Bare('I') => stdin = true,
                        CompactFlag::Bare('k') => keep_alive_on_exit = true,
                        CompactFlag::Bare('P') if allow_print_flags => print_target = true,
                        CompactFlag::Bare('P') => {
                            return Err(unsupported_flag("split-window", "-P"));
                        }
                        CompactFlag::Bare('v') => {
                            if direction_set {
                                return Err(RmuxError::Server(
                                    "split-window accepts only one of -h or -v".to_owned(),
                                ));
                            }
                            direction = SplitDirection::Vertical;
                            direction_set = true;
                        }
                        CompactFlag::Bare('Z') => preserve_zoom = true,
                        compact_flag @ CompactFlag::Value { flag: 'c', .. } => {
                            start_directory = Some(std::path::PathBuf::from(
                                compact_flag.value_or_next(&mut args, "-c start-directory")?,
                            ));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'e', .. } => {
                            environment
                                .push(compact_flag.value_or_next(&mut args, "-e name=value")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'F', .. }
                            if allow_print_flags =>
                        {
                            format = Some(compact_flag.value_or_next(&mut args, "-F format")?);
                        }
                        CompactFlag::Value { flag: 'F', .. } => {
                            return Err(unsupported_flag("split-window", "-F"));
                        }
                        compact_flag @ CompactFlag::Value { flag: 'l', .. } => {
                            size = Some(compact_flag.value_or_next(&mut args, "-l size")?);
                        }
                        compact_flag @ CompactFlag::Value { flag: 'p', .. } => {
                            let percentage = compact_flag.value_or_next(&mut args, "-p size")?;
                            if size.is_none() {
                                percentage_size = Some(percentage);
                            }
                        }
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_split_window_target(
                                compact_flag.value_or_next(&mut args, "-t target")?,
                            )?);
                        }
                        CompactFlag::Bare(flag) | CompactFlag::Value { flag, .. } => {
                            return Err(unsupported_flag("split-window", &format!("-{flag}")));
                        }
                    }
                }
            }
        }
    }
    let command = (!args.is_empty()).then_some(args.remaining());
    let stdin_to_empty_pane = stdin && command.is_none();
    if size.is_none() {
        if let Some(percentage) = percentage_size {
            size = Some(format!(
                "{}%",
                parse_percentage("split-window", "-p", &percentage)?
            ));
        }
    }
    let target = target.unwrap_or(implicit_split_target(
        sessions,
        find_context,
        "split-window",
    )?);

    let request = if command.is_some()
        || start_directory.is_some()
        || detached
        || size.is_some()
        || full_size
        || preserve_zoom
        || keep_alive_on_exit
        || stdin_to_empty_pane
    {
        Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target,
            direction,
            before,
            environment: (!environment.is_empty()).then_some(environment),
            command,
            process_command: None,
            start_directory,
            keep_alive_on_exit: None,
            detached,
            size,
            preserve_zoom,
            full_size,
            stdin_payload: stdin_to_empty_pane.then(Vec::new),
        }))
    } else {
        Request::SplitWindow(SplitWindowRequest {
            target,
            direction,
            before,
            environment: (!environment.is_empty()).then_some(environment),
        })
    };

    Ok(ParsedSplitWindowCommand {
        request,
        exit_policy: keep_alive_on_exit
            .then_some(crate::pane_terminals::SplitExitPolicyOverride::Key),
        print_target,
        format: format.unwrap_or_else(|| DEFAULT_SPLIT_WINDOW_PRINT_FORMAT.to_owned()),
    })
}

fn legacy_percentage_attached_value(token: &str) -> Option<&str> {
    token
        .strip_prefix("-p")
        .filter(|value| !value.is_empty() && !token.starts_with("--"))
}
