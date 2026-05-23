use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::{SelectPaneDirection, SplitDirection};

use super::{parse_command_args, parse_target_spec, TargetSpec};

pub(super) fn parse_select_pane_args(
    arguments: Vec<String>,
) -> Result<SelectPaneArgs, clap::Error> {
    parse_command_args::<SelectPaneArgs>("select-pane", arguments)?.validate()
}

pub(super) fn parse_resize_pane_args(
    arguments: Vec<String>,
) -> Result<ResizePaneArgs, clap::Error> {
    validate_resize_pane_tmux_direction_delta_syntax(&arguments)?;
    parse_command_args::<ResizePaneArgs>(
        "resize-pane",
        normalize_resize_pane_optional_delta(arguments),
    )
    .and_then(ResizePaneArgs::validate)
}

fn validate_resize_pane_tmux_direction_delta_syntax(
    arguments: &[String],
) -> Result<(), clap::Error> {
    for (index, argument) in arguments.iter().enumerate() {
        if matches!(argument.as_str(), "-D" | "-U" | "-L" | "-R") {
            if arguments
                .get(index + 1)
                .is_some_and(|next| next.parse::<u16>().is_ok())
                && index + 2 < arguments.len()
            {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::UnknownArgument,
                    format!("unexpected argument '{}'", arguments[index + 1]),
                ));
            }
        } else if matches!(argument.get(..3), Some("-D=" | "-U=" | "-L=" | "-R=")) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                format!("unexpected argument '{argument}'"),
            ));
        }
    }

    Ok(())
}

fn normalize_resize_pane_optional_delta(arguments: Vec<String>) -> Vec<String> {
    let Some(direction_index) = arguments
        .iter()
        .position(|argument| matches!(argument.as_str(), "-D" | "-U" | "-L" | "-R"))
    else {
        return arguments;
    };

    let direction = arguments[direction_index].clone();
    if arguments
        .iter()
        .skip(direction_index + 1)
        .any(|argument| matches!(argument.as_str(), "-D" | "-U" | "-L" | "-R"))
    {
        return arguments;
    }

    if arguments
        .get(direction_index + 1)
        .is_some_and(|next| next.parse::<u16>().is_ok())
        && !arguments
            .iter()
            .skip(direction_index + 2)
            .any(|argument| argument.starts_with('-'))
    {
        let mut normalized = arguments;
        let value = normalized.remove(direction_index + 1);
        normalized[direction_index] = format!("{direction}={value}");
        return normalized;
    }

    if arguments
        .last()
        .is_some_and(|last| last.parse::<u16>().is_ok())
        && arguments.len() > direction_index + 1
    {
        let mut normalized = arguments;
        let value = normalized.pop().expect("last resize-pane delta must exist");
        normalized[direction_index] = format!("{direction}={value}");
        return normalized;
    }

    arguments
}

#[derive(Debug, Clone, Args)]
#[command(disable_help_flag = true, group(
    ArgGroup::new("direction")
        .required(false)
        .multiple(false)
        .args(["horizontal", "vertical"])
))]
pub(crate) struct SplitWindowArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'h', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) horizontal: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) vertical: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 'l', allow_hyphen_values = true)]
    pub(crate) size: Option<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_target: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("relative")
        .required(false)
        .multiple(false)
        .args(["down", "up"])
))]
pub(crate) struct SwapPaneArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'D', action = ArgAction::SetTrue, group = "relative")]
    pub(crate) down: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue, group = "relative")]
    pub(crate) up: bool,
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) preserve_zoom: bool,
}

#[derive(Debug, Clone, Args)]
#[command(disable_help_flag = true, group(
    ArgGroup::new("direction")
        .required(false)
        .multiple(false)
        .args(["horizontal", "vertical"])
), group(
    ArgGroup::new("size_spec")
        .required(false)
        .multiple(false)
        .args(["size", "percentage"])
))]
pub(crate) struct JoinPaneArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) before: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'f', action = ArgAction::SetTrue)]
    pub(crate) full_size: bool,
    #[arg(short = 'h', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) horizontal: bool,
    #[arg(short = 'l', allow_hyphen_values = true)]
    pub(crate) size: Option<String>,
    #[arg(short = 'p')]
    pub(crate) percentage: Option<u8>,
    #[arg(short = 'v', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) vertical: bool,
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: TargetSpec,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("placement")
        .required(false)
        .multiple(false)
        .args(["after", "before"])
))]
pub(crate) struct BreakPaneArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) after: bool,
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) before: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'F')]
    pub(crate) format: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_target: bool,
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'n')]
    pub(crate) name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct PipePaneArgs {
    #[arg(short = 'I', action = ArgAction::SetTrue)]
    pub(crate) stdin: bool,
    #[arg(short = 'O', action = ArgAction::SetTrue)]
    pub(crate) stdout: bool,
    #[arg(short = 'o', action = ArgAction::SetTrue)]
    pub(crate) once: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct RespawnPaneArgs {
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) kill: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SelectLayoutArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    pub(crate) layout: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ResizePaneArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'D', num_args = 0..=1, default_missing_value = "1")]
    pub(crate) down: Option<u16>,
    #[arg(short = 'U', num_args = 0..=1, default_missing_value = "1")]
    pub(crate) up: Option<u16>,
    #[arg(short = 'L', num_args = 0..=1, default_missing_value = "1")]
    pub(crate) left: Option<u16>,
    #[arg(short = 'R', num_args = 0..=1, default_missing_value = "1")]
    pub(crate) right: Option<u16>,
    #[arg(short = 'x')]
    pub(crate) columns: Option<u16>,
    #[arg(short = 'y')]
    pub(crate) rows: Option<u16>,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
}

impl ResizePaneArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        let relative_count = [
            self.down.is_some(),
            self.up.is_some(),
            self.left.is_some(),
            self.right.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        let absolute_count = usize::from(self.columns.is_some()) + usize::from(self.rows.is_some());
        let invalid = relative_count > 1
            || (self.zoom && (relative_count > 0 || absolute_count > 0))
            || (relative_count > 0 && absolute_count > 0);
        if invalid {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "resize-pane accepts only one relative adjustment, zoom, or absolute size",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct PaneTargetArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_all_except: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("marking")
        .required(false)
        .multiple(false)
        .args(["mark", "clear_marked"])
), group(
    ArgGroup::new("direction")
        .required(false)
        .multiple(false)
        .args(["up", "down", "left", "right"])
))]
pub(crate) struct SelectPaneArgs {
    #[arg(short = 'm', action = ArgAction::SetTrue, group = "marking")]
    pub(crate) mark: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue, group = "marking")]
    pub(crate) clear_marked: bool,
    #[arg(short = 'U', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) up: bool,
    #[arg(short = 'D', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) down: bool,
    #[arg(short = 'L', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) left: bool,
    #[arg(short = 'R', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) right: bool,
    #[arg(short = 'T')]
    pub(crate) title: Option<String>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct CopyModeArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) page_down: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue)]
    pub(crate) exit_on_scroll: bool,
    #[arg(short = 'H', action = ArgAction::SetTrue)]
    pub(crate) hide_position: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse_drag_start: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) cancel_mode: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue)]
    pub(crate) scrollbar_scroll: bool,
    #[arg(short = 's', value_parser = parse_target_spec)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) page_up: bool,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ClockModeArgs {
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct DisplayPanesArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) non_blocking: bool,
    #[arg(short = 'd')]
    pub(crate) duration_ms: Option<u64>,
    #[arg(short = 'N', action = ArgAction::SetTrue)]
    pub(crate) no_command: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListPanesArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_sessions: bool,
    #[arg(short = 's', action = ArgAction::SetTrue)]
    pub(crate) short_format: bool,
    #[arg(short = 't', value_parser = parse_target_spec)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'F')]
    pub(crate) format: Option<String>,
}

impl SplitWindowArgs {
    pub(crate) fn direction(&self) -> SplitDirection {
        if self.horizontal {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        }
    }
}

impl SwapPaneArgs {
    pub(crate) fn uses_relative_target(&self) -> bool {
        self.down || self.up
    }
}

impl JoinPaneArgs {
    pub(crate) fn direction(&self) -> SplitDirection {
        if self.horizontal {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        }
    }

    pub(crate) fn size_spec(&self) -> Option<String> {
        self.percentage
            .map(|value| format!("{value}%"))
            .or_else(|| self.size.clone())
    }
}

impl SelectPaneArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        if self.direction().is_some() && (self.mark || self.clear_marked || self.title.is_some()) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "select-pane -U/-D/-L/-R cannot be combined with -m, -M, or -T",
            ));
        }

        Ok(self)
    }

    pub(crate) fn direction(&self) -> Option<SelectPaneDirection> {
        if self.up {
            Some(SelectPaneDirection::Up)
        } else if self.down {
            Some(SelectPaneDirection::Down)
        } else if self.left {
            Some(SelectPaneDirection::Left)
        } else if self.right {
            Some(SelectPaneDirection::Right)
        } else {
            None
        }
    }
}

impl DisplayPanesArgs {
    pub(crate) fn template_command(&self) -> Option<String> {
        if self.template.is_empty() {
            None
        } else {
            Some(self.template.join(" "))
        }
    }
}
