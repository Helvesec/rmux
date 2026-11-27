//! tmux's parse-time argument check (`args_parse`) and display form
//! (`args_print`, `cmd_list_print`), applied to a parsed command.
//!
//! tmux rejects a bad flag or a wrong positional count while it builds the
//! command list, before any command of the input runs. Callers that need that
//! classification (control-mode lines) run this check on every command of the
//! input, nested brace blocks included.
//!
//! tmux also keeps a hook as the command list it parsed, so `show-hooks`
//! prints that list back rather than the text it was given; the same option
//! scan drives that display form.

use std::collections::BTreeMap;
use std::fmt;

use super::reparse::escape_argument_for_reparse;
use super::{CommandArgument, ParsedCommand, ParsedCommands};
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
    match TmuxArgumentTemplate::for_command(command.name()) {
        Some(template) => split_tmux_arguments(template, command).map(|_| ()),
        None => Ok(()),
    }
}

/// Choosers take one optional template, not a tail to join into a command.
/// Reject extra templates at parse time, including in stored command lists.
/// Leave invalid options to the caller's existing diagnostic path (including
/// CLI help); only a successful option scan establishes positional arity.
pub(super) fn check_choose_template_arity(
    command: &ParsedCommand,
) -> Result<(), TmuxArgumentError> {
    if !matches!(
        command.name(),
        "choose-tree" | "choose-client" | "choose-buffer"
    ) {
        return Ok(());
    }
    let template = TmuxArgumentTemplate::for_command(command.name())
        .expect("chooser argument template exists");
    let Ok(scan) = scan_options(command.name(), template, command.arguments()) else {
        return Ok(());
    };
    if let Some(upper) = template
        .upper
        .filter(|upper| command.arguments().len() - scan.positional_start > *upper)
    {
        return Err(TmuxArgumentError(format!(
            "command {}: too many arguments (need at most {upper})",
            command.name()
        )));
    }
    Ok(())
}

/// One command's arguments as tmux's `args_parse` accepts them: the flags in
/// input order (a `--` terminator consumed), then the positionals, whose count
/// fits the template.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TmuxArguments<'a> {
    flags: Vec<TmuxFlag<'a>>,
    positionals: &'a [CommandArgument],
}

fn split_tmux_arguments<'a>(
    template: &TmuxArgumentTemplate,
    command: &'a ParsedCommand,
) -> Result<TmuxArguments<'a>, TmuxArgumentError> {
    let name = command.name();
    let error = |cause: String| TmuxArgumentError(format!("command {name}: {cause}"));
    let scan = scan_options(name, template, command.arguments())?;

    let positionals = &command.arguments()[scan.positional_start..];
    if positionals.len() < template.lower {
        return Err(error(format!(
            "too few arguments (need at least {})",
            template.lower
        )));
    }
    if let Some(upper) = template.upper.filter(|upper| positionals.len() > *upper) {
        return Err(error(format!("too many arguments (need at most {upper})")));
    }
    Ok(TmuxArguments {
        flags: scan.flags,
        positionals,
    })
}

/// Removes a `--` that ends the options and has nothing after it.
///
/// tmux's `args_parse` consumes that `--` like getopt, so `list-sessions --`
/// runs as `list-sessions`. Only the option terminator is removed: a `--`
/// that is the value of a flag (`list-sessions -F --`) or that comes after
/// the first positional stays an argument. A command without a tmux template,
/// or whose options tmux would reject, is returned unchanged so that its
/// parser still reports the same error.
#[must_use]
pub fn without_trailing_option_terminator(command: ParsedCommand) -> ParsedCommand {
    let Some(template) = TmuxArgumentTemplate::for_command(command.name()) else {
        return command;
    };
    let Ok(scan) = scan_options(command.name(), template, command.arguments()) else {
        return command;
    };
    if scan
        .terminator
        .is_none_or(|index| index + 1 != command.arguments().len())
    {
        return command;
    }
    let mut arguments = command.arguments().to_vec();
    arguments.pop();
    command.with_arguments(arguments)
}

/// One flag occurrence as tmux's `args_parse` records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TmuxFlag<'a> {
    Boolean(char),
    Value(char, &'a str),
}

/// Where tmux's option scan stopped in one command's arguments.
struct OptionScan<'a> {
    /// The flags read before the scan stopped, in input order.
    flags: Vec<TmuxFlag<'a>>,
    /// Index of the first positional argument.
    positional_start: usize,
    /// Index of the `--` that ended the options, when one did.
    terminator: Option<usize>,
}

/// Walks the leading flags as tmux's `args_parse` does, stopping at the first
/// positional, at a lone `-`, or after a `--` terminator.
fn scan_options<'a>(
    name: &str,
    template: &TmuxArgumentTemplate,
    arguments: &'a [CommandArgument],
) -> Result<OptionScan<'a>, TmuxArgumentError> {
    let error = |cause: String| TmuxArgumentError(format!("command {name}: {cause}"));
    let mut flags = Vec::new();
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
            return Ok(OptionScan {
                flags,
                positional_start: index,
                terminator: Some(index - 1),
            });
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
                Some(FlagKind::Boolean) => {
                    flags.push(TmuxFlag::Boolean(flag));
                    continue;
                }
                Some(FlagKind::Value) => {}
            }
            let attached = &cluster[offset + flag.len_utf8()..];
            if attached.is_empty() {
                match arguments.get(index) {
                    None => return Err(error(format!("-{flag} expects an argument"))),
                    Some(CommandArgument::Commands(_)) => {
                        return Err(error(format!("-{flag} argument must be a string")));
                    }
                    Some(CommandArgument::String(value)) => {
                        flags.push(TmuxFlag::Value(flag, value));
                        index += 1;
                    }
                }
            } else {
                flags.push(TmuxFlag::Value(flag, attached));
            }
            break;
        }
    }

    Ok(OptionScan {
        flags,
        positional_start: index,
        terminator: None,
    })
}

/// Renders `commands` the way tmux 3.7b's `cmd_list_print(list, 0)` shows a
/// hook in `show-hooks`, for example `list-sessions -F x --` as
/// `list-sessions -F x` and `new-window -n foo -d` as `new-window -d -n foo`.
///
/// One deliberate divergence, C-D98: tmux drops the terminator even when the
/// first positional starts with `-` (`display-message -- -x` prints as
/// `display-message -x`, which reads back as a flag). RMUX keeps `--` there
/// so the shown command means what the hook runs.
///
/// A command without a tmux template (an RMUX extension) or whose arguments
/// tmux would reject keeps its words in order; nested blocks are still
/// rendered this way.
#[must_use]
pub fn print_tmux_command_list(commands: &ParsedCommands) -> String {
    commands
        .commands()
        .iter()
        .map(print_tmux_command)
        .collect::<Vec<_>>()
        .join(" ; ")
}

fn print_tmux_command(command: &ParsedCommand) -> String {
    let split = TmuxArgumentTemplate::for_command(command.name())
        .and_then(|template| split_tmux_arguments(template, command).ok());
    let mut words = vec![command.name().to_owned()];
    match split {
        Some(arguments) => push_tmux_arguments(&mut words, &arguments),
        None => words.extend(command.arguments().iter().map(print_argument)),
    }
    words.join(" ")
}

fn push_tmux_arguments(words: &mut Vec<String>, arguments: &TmuxArguments<'_>) {
    let mut booleans = BTreeMap::<char, usize>::new();
    let mut values = BTreeMap::<char, Vec<&str>>::new();
    for flag in &arguments.flags {
        match *flag {
            TmuxFlag::Boolean(flag) => *booleans.entry(flag).or_default() += 1,
            TmuxFlag::Value(flag, value) => values.entry(flag).or_default().push(value),
        }
    }

    if !booleans.is_empty() {
        let cluster = booleans
            .iter()
            .flat_map(|(flag, count)| std::iter::repeat_n(*flag, *count))
            .collect::<String>();
        words.push(format!("-{cluster}"));
    }
    for (flag, flag_values) in &values {
        for value in flag_values {
            words.push(format!("-{flag}"));
            words.push(escape_argument_for_reparse(value));
        }
    }
    if arguments
        .positionals
        .first()
        .is_some_and(reads_back_as_flag_cluster)
    {
        words.push("--".to_owned());
    }
    words.extend(arguments.positionals.iter().map(print_argument));
}

fn reads_back_as_flag_cluster(argument: &CommandArgument) -> bool {
    argument
        .as_string()
        .is_some_and(|value| value.len() > 1 && value.starts_with('-'))
}

fn print_argument(argument: &CommandArgument) -> String {
    match argument {
        CommandArgument::String(value) => escape_argument_for_reparse(value),
        CommandArgument::Commands(commands) => {
            format!("{{ {} }}", print_tmux_command_list(commands))
        }
    }
}

#[cfg(test)]
mod tests;
