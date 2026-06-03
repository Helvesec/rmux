use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::RotateWindowDirection;

use super::{parse_target_spec, QueuedCommand, TargetSpec};

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
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'n')]
    pub(crate) name: Option<String>,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct KillWindowArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_others: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct WindowTargetArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RenameWindowArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) new_name: String,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListWindowsArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_sessions: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'F')]
    pub(crate) format: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct MoveWindowArgs {
    #[arg(short = 'r', action = ArgAction::SetTrue)]
    pub(crate) reindex: bool,
    #[arg(short = 'k', action = ArgAction::SetTrue, conflicts_with = "reindex")]
    pub(crate) kill_target: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 's', value_parser = parse_target_spec, conflicts_with = "reindex")]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SwapWindowArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
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
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: TargetSpec,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct UnlinkWindowArgs {
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill_if_last: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
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
    #[arg(short = 't', value_parser = parse_target_spec)]
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
pub(crate) struct ResizeWindowArgs {
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
    #[arg(short = 't', value_parser = parse_target_spec)]
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
    #[arg(short = 't', value_parser = parse_target_spec)]
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
