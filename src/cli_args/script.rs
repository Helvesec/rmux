use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::WaitForMode;

use super::{parse_command_args, parse_target_spec, QueuedCommand, TargetSpec};

pub(super) fn parse_source_file_args(
    arguments: Vec<String>,
) -> Result<SourceFileArgs, clap::Error> {
    validate_source_file_options(&arguments)?;
    parse_command_args("source-file", arguments)
}

fn validate_source_file_options(arguments: &[String]) -> Result<(), clap::Error> {
    let mut expect_target = false;
    for argument in arguments {
        if expect_target {
            expect_target = false;
            continue;
        }
        if argument == "--" {
            break;
        }
        if argument == "-" || !argument.starts_with('-') {
            break;
        }
        if let Some(long) = argument.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            if name != "help" {
                return Err(source_file_unknown_flag(format!("--{name}")));
            }
            continue;
        }

        let mut chars = argument[1..].chars().peekable();
        while let Some(flag) = chars.next() {
            match flag {
                'F' | 'n' | 'q' | 'v' => {}
                't' => {
                    expect_target = chars.peek().is_none();
                    break;
                }
                _ => return Err(source_file_unknown_flag(format!("-{flag}"))),
            }
        }
    }
    Ok(())
}

fn source_file_unknown_flag(flag: String) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command source-file: unknown flag {flag}"),
    )
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RunShellArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) background: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) as_commands: bool,
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    pub(crate) show_stderr: bool,
    #[arg(short = 'd')]
    pub(crate) delay_seconds: Option<f64>,
    #[arg(short = 'c')]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

impl RunShellArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SourceFileArgs {
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) expand_paths: bool,
    #[arg(short = 'n', action = ArgAction::SetTrue)]
    pub(crate) parse_only: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) quiet: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue)]
    pub(crate) verbose: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(required = true, allow_hyphen_values = true)]
    pub(crate) paths: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct IfShellArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) background: bool,
    #[arg(short = 'F', action = ArgAction::SetTrue)]
    pub(crate) format_mode: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) condition: String,
    #[arg(allow_hyphen_values = true)]
    pub(crate) then_command: String,
    #[arg(allow_hyphen_values = true)]
    pub(crate) else_command: Option<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for IfShellArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("mode")
        .required(false)
        .multiple(false)
        .args(["signal", "lock", "unlock"])
))]
pub(crate) struct WaitForArgs {
    #[arg(short = 'S', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) signal: bool,
    #[arg(short = 'L', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) lock: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue, group = "mode")]
    pub(crate) unlock: bool,
    #[arg(allow_hyphen_values = true)]
    pub(crate) channel: String,
}

impl WaitForArgs {
    pub(crate) fn mode(&self) -> WaitForMode {
        if self.signal {
            WaitForMode::Signal
        } else if self.lock {
            WaitForMode::Lock
        } else if self.unlock {
            WaitForMode::Unlock
        } else {
            WaitForMode::Wait
        }
    }
}
