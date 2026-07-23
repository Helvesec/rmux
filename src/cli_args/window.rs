use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::RotateWindowDirection;

use super::{parse_command_args, parse_target_spec, QueuedCommand, TargetSpec};

pub(super) fn parse_rename_window_args(
    arguments: Vec<String>,
) -> Result<RenameWindowArgs, clap::Error> {
    parse_command_args::<RawRenameWindowArgs>("rename-window", arguments)?.validate()
}

pub(super) fn parse_select_window_args(
    arguments: Vec<String>,
) -> Result<SelectWindowArgs, clap::Error> {
    parse_command_args::<SelectWindowArgs>("select-window", arguments)?.validate()
}

pub(super) fn parse_swap_window_args(
    arguments: Vec<String>,
) -> Result<SwapWindowArgs, clap::Error> {
    parse_command_args::<SwapWindowArgs>("swap-window", arguments)?.validate()
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("placement")
        .required(false)
        .multiple(false)
        .args(["after", "before"])
))]
pub(crate) struct NewWindowArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) after: bool,
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) before: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_target: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill_existing: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    pub(crate) select_existing: bool,
    #[arg(short = 'n')]
    pub(crate) name: Option<String>,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for NewWindowArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct KillWindowArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_others: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct WindowTargetArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("navigation")
        .required(false)
        .multiple(false)
        .args(["last", "next", "previous"])
))]
pub(crate) struct SelectWindowArgs {
    #[arg(short = 'l', action = ArgAction::SetTrue, group = "navigation")]
    pub(crate) last: bool,
    #[arg(short = 'n', action = ArgAction::SetTrue, group = "navigation")]
    pub(crate) next: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue, group = "navigation")]
    pub(crate) previous: bool,
    #[arg(short = 'T', action = ArgAction::SetTrue)]
    pub(crate) toggle_last: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue, hide = true)]
    reject_zoom: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

impl SelectWindowArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        if self.reject_zoom {
            return Err(tmux_unknown_flag_error("select-window", "-Z"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RenameWindowArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) new_name: String,
}

#[derive(Debug, Clone, Args)]
struct RawRenameWindowArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, num_args = 1..)]
    names: Vec<String>,
}

impl RawRenameWindowArgs {
    fn validate(self) -> Result<RenameWindowArgs, clap::Error> {
        match self.names.as_slice() {
            [new_name] => Ok(RenameWindowArgs {
                target: self.target,
                new_name: new_name.clone(),
            }),
            [] => Err(clap::Error::raw(
                clap::error::ErrorKind::TooFewValues,
                "command rename-window: too few arguments (need at least 1)",
            )),
            _ => Err(clap::Error::raw(
                clap::error::ErrorKind::TooManyValues,
                "command rename-window: too many arguments (need at most 1)",
            )),
        }
    }
}

fn tmux_unknown_flag_error(command_name: &str, flag: &str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: unknown flag {flag}"),
    )
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListWindowsArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_sessions: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'F', conflicts_with = "json")]
    pub(crate) format: Option<String>,
    #[arg(long = "json", action = ArgAction::SetTrue)]
    pub(crate) json: bool,
    #[arg(short = 'f', allow_hyphen_values = true)]
    pub(crate) filter: Option<String>,
    #[arg(short = 'O')]
    pub(crate) sort_order: Option<String>,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reversed: bool,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("position")
        .required(false)
        .multiple(false)
        .args(["after", "before"])
))]
pub(crate) struct MoveWindowArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue, group = "position")]
    pub(crate) after: bool,
    #[arg(short = 'b', action = ArgAction::SetTrue, group = "position")]
    pub(crate) before: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reindex: bool,
    #[arg(short = 'k', action = ArgAction::SetTrue, conflicts_with = "reindex")]
    pub(crate) kill_target: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SwapWindowArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue, hide = true)]
    reject_after: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

impl SwapWindowArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        if self.reject_after {
            return Err(tmux_unknown_flag_error("swap-window", "-a"));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("position")
        .required(false)
        .multiple(false)
        .args(["after", "before"])
))]
pub(crate) struct LinkWindowArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue, group = "position")]
    pub(crate) after: bool,
    #[arg(short = 'b', action = ArgAction::SetTrue, group = "position")]
    pub(crate) before: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill_target: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct UnlinkWindowArgs {
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill_if_last: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("direction")
        .required(false)
        .multiple(false)
        .args(["down", "up"])
))]
pub(crate) struct RotateWindowArgs {
    #[arg(short = 'D', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) down: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) up: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) restore_zoom: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

impl RotateWindowArgs {
    pub(crate) fn direction(&self) -> RotateWindowDirection {
        if self.down {
            RotateWindowDirection::Down
        } else {
            RotateWindowDirection::Up
        }
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("balanced")
        .required(false)
        .multiple(false)
        .args(["expand", "shrink"])
))]
pub(crate) struct ResizeWindowArgs {
    #[arg(short = 'A', action = ArgAction::SetTrue, group = "balanced")]
    pub(crate) expand: bool,
    #[arg(short = 'a', action = ArgAction::SetTrue, group = "balanced")]
    pub(crate) shrink: bool,
    #[arg(short = 'D', action = ArgAction::SetTrue)]
    pub(crate) down: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue)]
    pub(crate) up: bool,
    #[arg(short = 'L', action = ArgAction::SetTrue)]
    pub(crate) left: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue)]
    pub(crate) right: bool,
    #[arg(short = 'x')]
    pub(crate) width: Option<u16>,
    #[arg(short = 'y')]
    pub(crate) height: Option<u16>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    /// Adjustment amount (default 1).
    pub(crate) adjustment: Option<u16>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RespawnWindowArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill: bool,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct FindWindowArgs {
    #[arg(short = 'i', action = ArgAction::SetTrue)]
    pub(crate) case_insensitive: bool,
    #[arg(short = 'C', action = ArgAction::SetTrue)]
    pub(crate) search_content: bool,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) search_name: bool,
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) regex: bool,
    #[arg(short = 'T', action = ArgAction::SetTrue)]
    pub(crate) search_title: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_pane: Option<String>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) match_string: String,
    #[arg(skip = String::new())]
    pub(crate) queue_command: String,
}

impl QueuedCommand for FindWindowArgs {
    fn set_queue_command(&mut self, queue_command: String) {
        self.queue_command = queue_command;
    }
}
