//! `resize-pane` argument parsing: raw-argument pre-validation, optional
//! delta normalization and the clap argument model.

use clap::{ArgAction, Args};
use rmux_core::tmux_precedence;

use super::{
    floating_position, parse_command_args, parse_target_spec,
    validate_required_absolute_resize_arguments, TargetSpec,
};

pub(in crate::cli_args) fn parse_resize_pane_args(
    arguments: Vec<String>,
) -> Result<ResizePaneArgs, clap::Error> {
    if floating_position::validate_occurrences(&arguments)? {
        return parse_command_args::<ResizePaneArgs>("resize-pane", arguments)
            .and_then(ResizePaneArgs::validate);
    }
    let arguments = tmux_precedence::normalize_tmux_precedence("resize-pane", arguments);
    validate_required_absolute_resize_arguments(&arguments)?;
    validate_resize_pane_tmux_direction_delta_syntax(&arguments)?;
    validate_resize_pane_absolute_size_values(&arguments)?;
    let arguments = normalize_resize_pane_optional_delta(arguments);
    let arguments = normalize_resize_pane_no_direction_trailing_adjustment(arguments)?;
    parse_command_args::<ResizePaneArgs>("resize-pane", arguments)
        .and_then(ResizePaneArgs::validate)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResizePaneSize {
    Cells(u16),
    Percent(u8),
    /// Height validation deferred until a preceding valid width is committed.
    DeferredError(&'static str),
}

impl ResizePaneSize {
    pub(crate) fn resolve(self, total: u16) -> Option<u16> {
        match self {
            Self::Cells(value) => Some(value),
            Self::DeferredError(_) => None,
            Self::Percent(value) => {
                let cells = u32::from(total) * u32::from(value) / 100;
                Some(u16::try_from(cells.max(1)).unwrap_or(u16::MAX))
            }
        }
    }
}

fn parse_resize_pane_height(value: &str) -> Result<ResizePaneSize, String> {
    match parse_resize_pane_size(value) {
        Ok(size) => Ok(size),
        Err(_) => {
            let number = value.strip_suffix('%').unwrap_or(value).parse::<i64>();
            let message = match number {
                Ok(n) if n < 0 => "height too small",
                Ok(n) if n > i64::from(i32::MAX) || (value.ends_with('%') && n > 100) => {
                    "height too large"
                }
                Err(error)
                    if *error.kind() == std::num::IntErrorKind::PosOverflow
                        && integer_like_resize_pane_adjustment(
                            value.strip_suffix('%').unwrap_or(value),
                        ) =>
                {
                    "height too large"
                }
                Err(error)
                    if *error.kind() == std::num::IntErrorKind::NegOverflow
                        && integer_like_resize_pane_adjustment(
                            value.strip_suffix('%').unwrap_or(value),
                        ) =>
                {
                    "height too small"
                }
                _ => "height invalid",
            };
            Ok(ResizePaneSize::DeferredError(message))
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
    let preceding_width = arguments
        .iter()
        .any(|arg| arg == "-x" || short_flag_attached_value(arg, "-x").is_some());
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
                    if !preceding_width {
                        validate_resize_pane_absolute_size_value(ResizePaneAxis::Height, value)?;
                    }
                }
                index += 2;
            }
            argument => {
                if let Some(value) = short_flag_attached_value(argument, "-x") {
                    validate_resize_pane_absolute_size_value(ResizePaneAxis::Width, value)?;
                } else if let Some(value) = short_flag_attached_value(argument, "-y") {
                    if !preceding_width {
                        validate_resize_pane_absolute_size_value(ResizePaneAxis::Height, value)?;
                    }
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
    let parsed = value.parse::<i64>().map_err(|error| {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!(
                "{} {}",
                axis.label(),
                match error.kind() {
                    std::num::IntErrorKind::PosOverflow
                        if integer_like_resize_pane_adjustment(value) =>
                        "too large",
                    std::num::IntErrorKind::NegOverflow
                        if integer_like_resize_pane_adjustment(value) =>
                        "too small",
                    _ => "invalid",
                }
            ),
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
    #[arg(short = 'y', value_parser = parse_resize_pane_height, allow_hyphen_values = true)]
    pub(crate) rows: Option<ResizePaneSize>,
    #[arg(short = 'X', value_parser = floating_position::parse_value, allow_hyphen_values = true)]
    pub(crate) position_x: Option<i32>,
    #[arg(short = 'Y', value_parser = floating_position::parse_value, allow_hyphen_values = true)]
    pub(crate) position_y: Option<i32>,
    #[arg(short = 'Z', action = ArgAction::SetTrue)]
    pub(crate) zoom: bool,
    #[arg(short = 'M', action = ArgAction::SetTrue)]
    pub(crate) mouse: bool,
    #[arg(short = 'T', action = ArgAction::SetTrue)]
    pub(crate) trim_below: bool,
}

impl ResizePaneArgs {
    fn validate(self) -> Result<Self, clap::Error> {
        if floating_position::validate_mode(&self)? {
            return Ok(self);
        }
        if self.columns.is_none() {
            if let Some(ResizePaneSize::DeferredError(message)) = self.rows {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::ValueValidation,
                    message,
                ));
            }
        }
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
