use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_proto::{SelectPaneDirection, SplitDirection};

use super::{parse_command_args, parse_target_spec, TargetSpec};

mod floating_position;

mod required_dimensions;
use required_dimensions::validate_required_absolute_resize_arguments;

mod resize_pane;
pub(super) use resize_pane::parse_resize_pane_args;
pub(crate) use resize_pane::{ResizePaneArgs, ResizePaneSize};

pub(super) fn parse_split_window_args(
    arguments: Vec<String>,
) -> Result<SplitWindowArgs, clap::Error> {
    validate_required_size_argument("split-window", &arguments)?;
    parse_command_args::<SplitWindowArgs>("split-window", arguments)?.validate()
}

pub(super) fn parse_join_pane_args(
    command_name: &'static str,
    arguments: Vec<String>,
) -> Result<JoinPaneArgs, clap::Error> {
    parse_command_args::<JoinPaneArgs>(command_name, arguments)?.validate(command_name)
}

pub(super) fn parse_select_pane_args(
    arguments: Vec<String>,
) -> Result<SelectPaneArgs, clap::Error> {
    parse_command_args::<SelectPaneArgs>("select-pane", arguments)?.validate()
}

pub(super) fn parse_select_layout_args(
    arguments: Vec<String>,
) -> Result<SelectLayoutArgs, clap::Error> {
    parse_command_args::<SelectLayoutArgs>("select-layout", arguments)
}

fn validate_required_size_argument(
    command_name: &'static str,
    arguments: &[String],
) -> Result<(), clap::Error> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--" {
            break;
        }
        if argument == "-l" {
            if arguments.get(index + 1).is_none() {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    format!("command {command_name}: -l expects an argument"),
                ));
            }
            index += 2;
            continue;
        }
        if split_window_option_takes_value(argument) {
            index += 2;
            continue;
        }
        if !argument.starts_with('-') {
            break;
        }
        index += 1;
    }
    Ok(())
}

fn split_window_option_takes_value(argument: &str) -> bool {
    matches!(argument, "-c" | "-e" | "-F" | "-p" | "-t")
}

#[derive(Debug, Clone, Args)]
#[command(disable_help_flag = true, group(
    ArgGroup::new("direction")
        .required(false)
        .multiple(false)
        .args(["horizontal", "vertical"])
    ))]
pub(crate) struct SplitWindowArgs {
    #[arg(short = 'b', action = ArgAction::SetTrue)]
    pub(crate) before: bool,
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) detached: bool,
    #[arg(short = 'e')]
    pub(crate) environment: Vec<String>,
    #[arg(short = 'f', action = ArgAction::SetTrue)]
    pub(crate) full_size: bool,
    #[arg(short = 'h', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) horizontal: bool,
    #[arg(short = 'v', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) vertical: bool,
    #[arg(short = 'c', allow_hyphen_values = true)]
    pub(crate) start_directory: Option<PathBuf>,
    #[arg(short = 'l', allow_hyphen_values = true, group = "size_spec")]
    pub(crate) size: Option<String>,
    #[arg(short = 'p', allow_hyphen_values = true)]
    pub(crate) percentage: Option<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_target: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) preserve_zoom: bool,
    #[arg(short = 'I', action = ArgAction::SetTrue)]
    pub(crate) stdin: bool,
    #[arg(short = 'k', action = ArgAction::SetTrue)]
    pub(crate) keep_alive_on_exit: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 'p', hide = true, allow_hyphen_values = true)]
    pub(crate) percentage: Option<String>,
    #[arg(short = 'v', action = ArgAction::SetTrue, group = "direction")]
    pub(crate) vertical: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct SelectLayoutArgs {
    #[arg(short = 'E', action = ArgAction::SetTrue)]
    pub(crate) spread: bool,
    #[arg(short = 'n', action = ArgAction::SetTrue)]
    pub(crate) next: bool,
    #[arg(short = 'o', action = ArgAction::SetTrue)]
    pub(crate) old: bool,
    #[arg(short = 'p', action = ArgAction::SetTrue)]
    pub(crate) previous: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    pub(crate) layout: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectLayoutMode {
    Next,
    Previous,
    Spread,
    Old,
}

impl SelectLayoutArgs {
    pub(crate) const fn mode(&self) -> Option<SelectLayoutMode> {
        if self.next {
            Some(SelectLayoutMode::Next)
        } else if self.previous {
            Some(SelectLayoutMode::Previous)
        } else if self.spread {
            Some(SelectLayoutMode::Spread)
        } else if self.old {
            Some(SelectLayoutMode::Old)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct LastPaneArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) disable_input: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue)]
    pub(crate) enable_input: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) keep_zoom: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct PaneTargetArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) kill_all_except: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 'l', action = ArgAction::SetTrue)]
    pub(crate) last: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) keep_zoom: bool,
    // tmux 3.7b accepts `-d` with `-e`; `-e` wins.
    #[arg(short = 'd', action = ArgAction::SetTrue)]
    pub(crate) disable_input: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue)]
    pub(crate) enable_input: bool,
    #[arg(short = 'T')]
    pub(crate) title: Option<String>,
    #[arg(short = 'P')]
    pub(crate) style: Option<String>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) page_up: bool,
}

impl CopyModeArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ClockModeArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
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
    #[arg(short = 't', allow_hyphen_values = true)]
    pub(crate) target_client: Option<String>,
    #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
    pub(crate) template: Vec<String>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ListPanesArgs {
    #[arg(short = 'a', action = ArgAction::SetTrue)]
    pub(crate) all_sessions: bool,
    #[arg(short = 's', action = ArgAction::SetTrue)]
    pub(crate) session_scope: bool,
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

impl SplitWindowArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        Ok(self)
    }

    pub(crate) fn direction(&self) -> SplitDirection {
        if self.horizontal {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        }
    }

    pub(crate) fn size_spec(&self) -> Option<String> {
        self.size
            .clone()
            .or_else(|| self.percentage.as_ref().map(|value| format!("{value}%")))
    }
}

impl SwapPaneArgs {
    pub(crate) fn uses_relative_target(&self) -> bool {
        self.down || self.up
    }
}

impl JoinPaneArgs {
    fn validate(self, _command_name: &'static str) -> Result<Self, clap::Error> {
        Ok(self)
    }

    pub(crate) fn direction(&self) -> SplitDirection {
        if self.horizontal {
            SplitDirection::Horizontal
        } else {
            SplitDirection::Vertical
        }
    }

    pub(crate) fn size_spec(&self) -> Option<String> {
        self.size
            .clone()
            .or_else(|| self.percentage.as_ref().map(|value| format!("{value}%")))
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
        if self.style.is_some()
            && (self.direction().is_some() || self.last || self.mark || self.clear_marked)
        {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "select-pane -P cannot be combined with -U, -D, -L, -R, -l, -m, or -M",
            ));
        }
        if self.last && (self.direction().is_some() || self.mark || self.clear_marked) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "select-pane -l cannot be combined with -U, -D, -L, -R, -m, or -M",
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

#[cfg(test)]
mod l1_034_tests;

#[cfg(test)]
#[test]
fn l1_040_mode_arguments_retain_width_and_deferred_height() {
    for width in ["10", "25%"] {
        for flags in [
            &["-Z"][..],
            &["-T"][..],
            &["-Z", "-T"][..],
            &["-T", "-Z"][..],
        ] {
            let mut argv = vec![
                "rmux",
                "resize-pane",
                "-t",
                "compat:0.0",
                "-x",
                width,
                "-ybad",
            ];
            argv.extend_from_slice(flags);
            let cli = crate::cli_args::parse(argv).unwrap();
            let crate::cli_args::Command::ResizePane(args) = cli.command.unwrap() else {
                panic!("resize")
            };
            assert_eq!(args.target.as_ref().unwrap().raw(), "compat:0.0");
            assert_eq!(args.zoom, flags.contains(&"-Z"));
            assert_eq!(args.trim_below, flags.contains(&"-T"));
            assert_eq!(
                args.columns,
                Some(if width == "10" {
                    ResizePaneSize::Cells(10)
                } else {
                    ResizePaneSize::Percent(25)
                })
            );
            assert_eq!(
                args.rows,
                Some(ResizePaneSize::DeferredError("height invalid"))
            );
            assert!(args.position_x.is_none() && args.position_y.is_none());
        }
    }
}

#[cfg(test)]
#[path = "pane/required_dimensions_tests.rs"]
mod required_dimensions_tests;
