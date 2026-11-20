//! tmux's parse-time argument check (`args_parse`), applied to a parsed command.
//!
//! tmux rejects a bad flag or a wrong positional count while it builds the
//! command list, before any command of the input runs. Callers that need that
//! classification (control-mode lines) run this check on every command of the
//! input, nested brace blocks included.

use std::fmt;

use super::{CommandArgument, ParsedCommand};
use crate::command_inventory::{command_short_option_spec, list_command_usage_without_alias};

mod templates;

use templates::TMUX_ARGUMENT_TEMPLATES;

/// The flags and positional bounds tmux 3.7b's `args_parse` accepts for one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TmuxArgumentTemplate {
    name: &'static str,
    boolean_flags: &'static str,
    value_flags: &'static str,
    lower: usize,
    upper: Option<usize>,
}

impl TmuxArgumentTemplate {
    const fn new(
        name: &'static str,
        boolean_flags: &'static str,
        value_flags: &'static str,
        lower: usize,
        upper: Option<usize>,
    ) -> Self {
        Self {
            name,
            boolean_flags,
            value_flags,
            lower,
            upper,
        }
    }

    fn for_command(name: &str) -> Option<&'static Self> {
        TMUX_ARGUMENT_TEMPLATES
            .iter()
            .find(|template| template.name == name)
    }

    /// Classifies `flag`, accepting the flags RMUX implements beyond tmux
    /// (for example `resize-pane -X`/`-Y`) with the kind RMUX gives them.
    fn flag_kind(&self, flag: char) -> Option<FlagKind> {
        if self.boolean_flags.contains(flag) {
            return Some(FlagKind::Boolean);
        }
        if self.value_flags.contains(flag) {
            return Some(FlagKind::Value);
        }
        let rmux_spec = command_short_option_spec(self.name)?;
        if rmux_spec.takes_value(flag) {
            Some(FlagKind::Value)
        } else if rmux_spec.is_boolean(flag) {
            Some(FlagKind::Boolean)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    Boolean,
    Value,
}

/// A command whose arguments tmux rejects before running anything.
///
/// The display is tmux's cause, for example
/// `command display-message: invalid flag -:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxArgumentError(String);

impl fmt::Display for TmuxArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for TmuxArgumentError {}

/// Checks one command's own arguments the way tmux 3.7b's `args_parse` does.
///
/// Nested brace blocks are not visited; the caller walks them first, as tmux
/// builds nested command lists before the command that carries them. A
/// command without a tmux template (an RMUX extension) is accepted unchanged.
pub fn check_tmux_arguments(command: &ParsedCommand) -> Result<(), TmuxArgumentError> {
    let name = command.name();
    let Some(template) = TmuxArgumentTemplate::for_command(name) else {
        return Ok(());
    };
    let error = |cause: String| TmuxArgumentError(format!("command {name}: {cause}"));
    let arguments = command.arguments();
    let mut index = 0;

    while let Some(CommandArgument::String(token)) = arguments.get(index) {
        let Some(cluster) = token
            .strip_prefix('-')
            .filter(|cluster| !cluster.is_empty())
        else {
            break;
        };
        index += 1;
        if cluster == "-" {
            break;
        }
        for (offset, flag) in cluster.char_indices() {
            if flag == '?' {
                return Err(TmuxArgumentError(format!(
                    "usage: {name} {}",
                    list_command_usage_without_alias(name)
                )));
            }
            if !flag.is_ascii_alphanumeric() {
                return Err(error(format!("invalid flag -{flag}")));
            }
            match template.flag_kind(flag) {
                None => return Err(error(format!("unknown flag -{flag}"))),
                Some(FlagKind::Boolean) => continue,
                Some(FlagKind::Value) => {}
            }
            if offset + flag.len_utf8() == cluster.len() {
                match arguments.get(index) {
                    None => return Err(error(format!("-{flag} expects an argument"))),
                    Some(CommandArgument::Commands(_)) => {
                        return Err(error(format!("-{flag} argument must be a string")));
                    }
                    Some(CommandArgument::String(_)) => index += 1,
                }
            }
            break;
        }
    }

    let positional = arguments.len() - index;
    if positional < template.lower {
        return Err(error(format!(
            "too few arguments (need at least {})",
            template.lower
        )));
    }
    if let Some(upper) = template.upper.filter(|upper| positional > *upper) {
        return Err(error(format!("too many arguments (need at most {upper})")));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
