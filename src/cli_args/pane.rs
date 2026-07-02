use std::path::PathBuf;

use clap::{ArgAction, ArgGroup, Args};
use rmux_core::tmux_precedence;
use rmux_proto::{SelectPaneDirection, SplitDirection};

use super::{parse_command_args, parse_target_spec, TargetSpec};

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
    parse_command_args::<SelectLayoutArgs>("select-layout", arguments)?.validate()
}

pub(super) fn parse_resize_pane_args(
    arguments: Vec<String>,
) -> Result<ResizePaneArgs, clap::Error> {
    let arguments = tmux_precedence::normalize_tmux_precedence("resize-pane", arguments);
    validate_required_absolute_resize_arguments(&arguments)?;
    validate_resize_pane_tmux_direction_delta_syntax(&arguments)?;
    validate_resize_pane_absolute_size_values(&arguments)?;
    let arguments = normalize_resize_pane_optional_delta(arguments);
    let arguments = normalize_resize_pane_no_direction_trailing_adjustment(arguments)?;
    parse_command_args::<ResizePaneArgs>("resize-pane", arguments)
        .and_then(ResizePaneArgs::validate)
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

fn validate_required_absolute_resize_arguments(arguments: &[String]) -> Result<(), clap::Error> {
    for (index, argument) in arguments.iter().enumerate() {
        let missing =
            matches!(argument.as_str(), "-x" | "-y") && arguments.get(index + 1).is_none();
        if missing {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                format!("command resize-pane: {argument} expects an argument"),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizePaneSize {
    Cells(u16),
    Percent(u8),
}

impl ResizePaneSize {
    pub(crate) fn resolve(self, total: u16) -> Option<u16> {
        match self {
            Self::Cells(value) => Some(value),
            Self::Percent(value) => {
                let cells = u32::from(total) * u32::from(value) / 100;
                Some(u16::try_from(cells.max(1)).unwrap_or(u16::MAX))
            }
        }
    }
}

fn parse_resize_pane_size(value: &str) -> Result<ResizePaneSize, String> {
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent
            .parse::<u8>()
            .map_err(|error| format!("invalid percentage: {error}"))?;
        if percent > 100 {
            return Err("percentage must be between 0 and 100".to_owned());
        }
        return Ok(ResizePaneSize::Percent(percent));
    }

    let cells = value
        .parse::<i64>()
        .map_err(|error| format!("invalid cell count: {error}"))?;
    if cells < 0 {
        return Err("cell count too small".to_owned());
    }
    if cells > i64::from(i32::MAX) {
        return Err("cell count too large".to_owned());
    }
    Ok(ResizePaneSize::Cells(clamp_resize_pane_cells(cells)))
}

fn parse_resize_pane_delta(value: &str) -> Result<u16, String> {
    let cells = parse_resize_pane_adjustment_integer(value)?;
    if cells <= 0 {
        return Err("adjustment too small".to_owned());
    }
    if cells > i128::from(i32::MAX) {
        return Err("adjustment too large".to_owned());
    }
    Ok(clamp_resize_pane_cells(cells as i64))
}

fn clamp_resize_pane_cells(cells: i64) -> u16 {
    u16::try_from(cells).unwrap_or(u16::MAX)
}

fn parse_resize_pane_adjustment_integer(value: &str) -> Result<i128, String> {
    match value.parse::<i128>() {
        Ok(value) => Ok(value),
        Err(_) if integer_like_resize_pane_adjustment(value) && value.starts_with('-') => {
            Err("adjustment too small".to_owned())
        }
        Err(_) if integer_like_resize_pane_adjustment(value) => {
            Err("adjustment too large".to_owned())
        }
        Err(error) => Err(format!("adjustment invalid: {error}")),
    }
}

fn validate_resize_pane_tmux_direction_delta_syntax(
    arguments: &[String],
) -> Result<(), clap::Error> {
    for (index, argument) in arguments.iter().enumerate() {
        if matches!(argument.as_str(), "-D" | "-U" | "-L" | "-R") {
            if let Some(next) = arguments
                .get(index + 1)
                .filter(|next| !next.starts_with('-'))
            {
                match parse_resize_pane_delta(next) {
                    Ok(_) => {}
                    Err(message) if message.starts_with("adjustment too large") => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            "adjustment too large",
                        ));
                    }
                    Err(message) if message.starts_with("adjustment too small") => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            "adjustment too small",
                        ));
                    }
                    Err(_) => {
                        return Err(clap::Error::raw(
                            clap::error::ErrorKind::ValueValidation,
                            "adjustment invalid",
                        ));
                    }
                }
            }
            if arguments
                .get(index + 1)
                .is_some_and(|next| parse_resize_pane_delta(next).is_ok())
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
        } else if let Some(flag) = attached_resize_pane_direction_flag(argument) {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                format!("command resize-pane: unknown flag -{flag}"),
            ));
        }
    }

    Ok(())
}

fn attached_resize_pane_direction_flag(argument: &str) -> Option<char> {
    for prefix in ["-D", "-U", "-L", "-R"] {
        if let Some(value) = argument.strip_prefix(prefix) {
            if value.is_empty() {
                return None;
            }
            return value.chars().next();
        }
    }
    None
}

#[derive(Clone, Copy)]
enum ResizePaneAxis {
    Width,
    Height,
}

impl ResizePaneAxis {
    const fn label(self) -> &'static str {
        match self {
            Self::Width => "width",
            Self::Height => "height",
        }
    }
}

fn validate_resize_pane_absolute_size_values(arguments: &[String]) -> Result<(), clap::Error> {
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "-x" => {
                if let Some(value) = arguments.get(index + 1) {
                    validate_resize_pane_absolute_size_value(ResizePaneAxis::Width, value)?;
                }
                index += 2;
            }
            "-y" => {
                if let Some(value) = arguments.get(index + 1) {
                    validate_resize_pane_absolute_size_value(ResizePaneAxis::Height, value)?;
                }
                index += 2;
            }
            argument => {
                if let Some(value) = short_flag_attached_value(argument, "-x") {
                    validate_resize_pane_absolute_size_value(ResizePaneAxis::Width, value)?;
                } else if let Some(value) = short_flag_attached_value(argument, "-y") {
                    validate_resize_pane_absolute_size_value(ResizePaneAxis::Height, value)?;
                }
                index += 1;
            }
        }
    }
    Ok(())
}

fn short_flag_attached_value<'a>(argument: &'a str, flag: &str) -> Option<&'a str> {
    let value = argument.strip_prefix(flag)?;
    if value.is_empty() {
        return None;
    }
    Some(value.strip_prefix('=').unwrap_or(value))
}

fn validate_resize_pane_absolute_size_value(
    axis: ResizePaneAxis,
    value: &str,
) -> Result<(), clap::Error> {
    let value = value.strip_suffix('%').unwrap_or(value);
    let value = value.strip_prefix('+').unwrap_or(value);
    let parsed = value.parse::<i64>().map_err(|_| {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("{} invalid", axis.label()),
        )
    })?;
    if parsed < 0 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("{} too small", axis.label()),
        ));
    }
    if parsed > i64::from(i32::MAX) {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("{} too large", axis.label()),
        ));
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
        return arguments;
    }

    if arguments
        .last()
        .is_some_and(|last| resize_pane_delta_candidate(last))
        && arguments.len() > direction_index + 1
        && resize_pane_has_standalone_trailing_delta(&arguments, direction_index)
    {
        let mut normalized = arguments;
        let value = normalized.pop().expect("last resize-pane delta must exist");
        normalized.insert(direction_index + 1, value);
        return normalized;
    }

    arguments
}

fn resize_pane_delta_candidate(value: &str) -> bool {
    integer_like_resize_pane_adjustment(value)
}

fn integer_like_resize_pane_adjustment(value: &str) -> bool {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn normalize_resize_pane_no_direction_trailing_adjustment(
    arguments: Vec<String>,
) -> Result<Vec<String>, clap::Error> {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "-D" | "-U" | "-L" | "-R"))
    {
        return Ok(arguments);
    }
    if !arguments
        .last()
        .is_some_and(|last| resize_pane_delta_candidate(last))
        || !resize_pane_has_standalone_trailing_delta_from(&arguments, 0)
    {
        return Ok(arguments);
    }

    let mut normalized = arguments;
    let value = normalized
        .pop()
        .expect("last resize-pane adjustment must exist");
    parse_resize_pane_delta(&value)
        .map_err(|message| clap::Error::raw(clap::error::ErrorKind::ValueValidation, message))?;
    Ok(normalized)
}

fn resize_pane_has_standalone_trailing_delta(arguments: &[String], direction_index: usize) -> bool {
    resize_pane_has_standalone_trailing_delta_from(arguments, direction_index + 1)
}

fn resize_pane_has_standalone_trailing_delta_from(
    arguments: &[String],
    start_index: usize,
) -> bool {
    let last_index = arguments.len().saturating_sub(1);
    let mut index = start_index;
    while index <= last_index {
        if index == last_index {
            return true;
        }

        match arguments[index].as_str() {
            "-t" | "-x" | "-y" => {
                if index + 1 == last_index {
                    return false;
                }
                index += 2;
            }
            "-M" | "-T" | "-Z" => {
                index += 1;
            }
            "-D" | "-U" | "-L" | "-R" => return false,
            argument
                if argument.starts_with("-t")
                    || argument.starts_with("-x")
                    || argument.starts_with("-y") =>
            {
                index += 1;
            }
            _ => return false,
        }
    }
    false
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
    #[arg(short = 'p', hide = true, allow_hyphen_values = true)]
    pub(crate) percentage: Option<String>,
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(short = 'P', action = ArgAction::SetTrue)]
    pub(crate) print_target: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) preserve_zoom: bool,
    #[arg(short = 'I', action = ArgAction::SetTrue)]
    pub(crate) stdin: bool,
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

impl SelectLayoutArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        let mode_count = [self.spread, self.next, self.old, self.previous]
            .into_iter()
            .filter(|present| *present)
            .count();
        if mode_count > 1 {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "select-layout accepts only one mode flag",
            ));
        }
        if mode_count == 1 && self.layout.is_some() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::TooManyValues,
                "command select-layout: too many arguments (need at most 0)",
            ));
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("input")
        .required(false)
        .multiple(false)
        .args(["disable_input", "enable_input"])
))]
pub(crate) struct LastPaneArgs {
    #[arg(short = 'd', action = ArgAction::SetTrue, group = "input")]
    pub(crate) disable_input: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue, group = "input")]
    pub(crate) enable_input: bool,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) keep_zoom: bool,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct ResizePaneArgs {
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'D', num_args = 0..=1, default_missing_value = "1", value_parser = parse_resize_pane_delta)]
    pub(crate) down: Option<u16>,
    #[arg(short = 'U', num_args = 0..=1, default_missing_value = "1", value_parser = parse_resize_pane_delta)]
    pub(crate) up: Option<u16>,
    #[arg(short = 'L', num_args = 0..=1, default_missing_value = "1", value_parser = parse_resize_pane_delta)]
    pub(crate) left: Option<u16>,
    #[arg(short = 'R', num_args = 0..=1, default_missing_value = "1", value_parser = parse_resize_pane_delta)]
    pub(crate) right: Option<u16>,
    #[arg(short = 'x', value_parser = parse_resize_pane_size, allow_hyphen_values = true)]
    pub(crate) columns: Option<ResizePaneSize>,
    #[arg(short = 'y', value_parser = parse_resize_pane_size, allow_hyphen_values = true)]
    pub(crate) rows: Option<ResizePaneSize>,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse: bool,
    #[arg(short = 'T', action = ArgAction::SetTrue)]
    pub(crate) trim_below: bool,
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
        let invalid = !self.zoom && !self.trim_below && relative_count > 1;
        if invalid {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "resize-pane accepts only one relative adjustment",
            ));
        }
        Ok(self)
    }
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
), group(
    ArgGroup::new("input")
        .required(false)
        .multiple(false)
        .args(["disable_input", "enable_input"])
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
    #[arg(short = 'd', action = ArgAction::SetTrue, group = "input")]
    pub(crate) disable_input: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue, group = "input")]
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
    #[arg(short = 'd', action = ArgAction::SetTrue, hide = true)]
    unsupported_page_down: bool,
    #[arg(short = 'e', action = ArgAction::SetTrue)]
    pub(crate) exit_on_scroll: bool,
    #[arg(short = 'H', action = ArgAction::SetTrue)]
    pub(crate) hide_position: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse_drag_start: bool,
    #[arg(short = 'q', action = ArgAction::SetTrue)]
    pub(crate) cancel_mode: bool,
    #[arg(short = 'S', action = ArgAction::SetTrue, hide = true)]
    unsupported_scrollbar_scroll: bool,
    #[arg(short = 's', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) source: Option<TargetSpec>,
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
    #[arg(short = 'u', action = ArgAction::SetTrue)]
    pub(crate) page_up: bool,
}

impl CopyModeArgs {
    pub(crate) fn validate(self) -> Result<Self, clap::Error> {
        if self.unsupported_page_down {
            return Err(unknown_flag_error("copy-mode", "-d"));
        }
        if self.unsupported_scrollbar_scroll {
            return Err(unknown_flag_error("copy-mode", "-S"));
        }
        Ok(self)
    }
}

fn unknown_flag_error(command_name: &str, flag: &str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::UnknownArgument,
        format!("command {command_name}: unknown flag {flag}"),
    )
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
    #[arg(short = 't', value_parser = parse_target_spec, allow_hyphen_values = true)]
    pub(crate) target: Option<TargetSpec>,
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
}

impl SplitWindowArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        if self.percentage.is_some() && self.size.is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                "size missing",
            ));
        }
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
        self.size.clone()
    }
}

impl SwapPaneArgs {
    pub(crate) fn uses_relative_target(&self) -> bool {
        self.down || self.up
    }
}

impl JoinPaneArgs {
    fn validate(self, command_name: &'static str) -> Result<Self, clap::Error> {
        if self.percentage.is_some() && self.size.is_none() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ValueValidation,
                format!("command {command_name}: size missing"),
            ));
        }
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
        self.size.clone()
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
