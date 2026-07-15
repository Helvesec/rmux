use std::path::{Path, PathBuf};

use rmux_proto::{
    IfShellRequest, Request, RmuxError, RunShellDelaySeconds, RunShellRequest, WaitForMode,
    WaitForRequest,
};

use super::tokens::{rebuild_shell_command, CommandTokens};
use super::values::{parse_non_negative_f64, reject_unknown_option_before_positional};
use super::{parse_pane_target, parse_target_arg};

pub(super) fn parse_run_shell(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut background = false;
    let mut as_commands = false;
    let mut show_stderr = false;
    let mut delay_seconds = None;
    let mut start_directory = None;
    let mut target = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        if let Some(flags) = args.optional_compact_flags("bCE") {
            for flag in flags {
                match flag {
                    'b' => background = true,
                    'C' => as_commands = true,
                    'E' => show_stderr = true,
                    _ => unreachable!("compact run-shell flags are prevalidated"),
                }
            }
            continue;
        }
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-b" => {
                let _ = args.optional();
                background = true;
            }
            "-C" => {
                let _ = args.optional();
                as_commands = true;
            }
            "-E" => {
                let _ = args.optional();
                show_stderr = true;
            }
            "-d" => {
                let _ = args.optional();
                delay_seconds = Some(parse_non_negative_f64(
                    "run-shell",
                    "-d",
                    &args.required("-d delay")?,
                )?);
            }
            flag if flag.starts_with("-d") && flag.len() > 2 => {
                let flag = args
                    .optional()
                    .expect("peeked run-shell -d<delay> flag must still be present");
                delay_seconds = Some(parse_non_negative_f64("run-shell", "-d", &flag[2..])?);
            }
            "-c" => {
                let _ = args.optional();
                start_directory = Some(PathBuf::from(args.required("-c start-directory")?));
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_pane_target("run-shell", args.required("-t target")?)?);
            }
            token => {
                reject_unknown_option_before_positional("run-shell", token)?;
                break;
            }
        }
    }
    let command_parts = args.remaining();
    let (command, arguments) = if command_parts.is_empty() {
        (String::new(), Vec::new())
    } else if as_commands {
        let mut command_parts = command_parts.into_iter();
        let command = command_parts
            .next()
            .expect("checked non-empty command parts");
        (command, Vec::new())
    } else {
        let mut command_parts = command_parts.into_iter();
        let command = rebuild_shell_command(vec![command_parts
            .next()
            .expect("checked non-empty command parts")]);
        (command, command_parts.collect())
    };

    Ok(Request::RunShell(Box::new(RunShellRequest {
        command,
        arguments,
        background,
        as_commands,
        show_stderr,
        delay_seconds: delay_seconds.map(RunShellDelaySeconds),
        start_directory,
        target,
        source_depth: None,
    })))
}

pub(super) fn parse_if_shell(
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
) -> Result<Request, RmuxError> {
    let mut format_mode = false;
    let mut background = false;
    let mut target = None;

    while let Some(token) = args.peek() {
        match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-b" => {
                let _ = args.optional();
                background = true;
            }
            "-F" => {
                let _ = args.optional();
                format_mode = true;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_target_arg("if-shell", args.required("-t target")?)?);
            }
            token => {
                reject_unknown_option_before_positional("if-shell", token)?;
                break;
            }
        }
    }

    let request = Request::IfShell(Box::new(IfShellRequest {
        condition: args.required("if-shell condition")?,
        format_mode,
        then_command: args.required("if-shell then command")?,
        else_command: args.optional(),
        target,
        caller_cwd: caller_cwd.map(Path::to_path_buf),
        background,
    }));
    args.no_extra("if-shell")?;
    Ok(request)
}

pub(super) fn parse_wait_for(mut args: CommandTokens) -> Result<Request, RmuxError> {
    let mut mode = WaitForMode::Wait;
    while let Some(token) = args.peek() {
        let next_mode = match token {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-S" => WaitForMode::Signal,
            "-L" => WaitForMode::Lock,
            "-U" => WaitForMode::Unlock,
            token => {
                reject_unknown_option_before_positional("wait-for", token)?;
                break;
            }
        };
        let _ = args.optional();
        if mode != WaitForMode::Wait {
            return Err(RmuxError::Server(
                "wait-for accepts only one of -S, -L, or -U".to_owned(),
            ));
        }
        mode = next_mode;
    }
    let channel = args.required("wait-for channel")?;
    args.no_extra("wait-for")?;

    Ok(Request::WaitFor(WaitForRequest { channel, mode }))
}
