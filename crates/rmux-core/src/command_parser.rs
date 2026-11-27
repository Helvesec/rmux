//! tmux-compatible command tokenization and command-name lookup.
//!
//! This module mirrors the frozen tmux `cmd-parse.y` lexer boundary closely
//! enough for RMUX command dispatch and config parsing to share one parser.
//! Frozen source anchors: `/opt/rmux/reference/tmux` at commit
//! `31d77e29b6c9fbb07d032018da78db3a8a38d979`, especially `cmd.c:121`
//! (`cmd_table[]`) and `cmd-parse.y:1053`, `cmd-parse.y:1201`,
//! `cmd-parse.y:1626` for argv parsing, continuation handling, and
//! tokenization.

use crate::{
    formats::{is_truthy, render_template, FormatVariable, FormatVariables},
    EnvironmentStore,
};

mod aliases;
mod argument_check;
pub use argument_check::{
    check_tmux_arguments, print_tmux_command_list, without_trailing_option_terminator,
    TmuxArgumentError,
};
mod control_size;
pub use control_size::ControlClientSize;
mod pan_adjustment;
pub use pan_adjustment::parse_pan_adjustment;
mod error;
mod escape;
mod grammar;
mod lexer;
mod lookup;
mod model;
mod reparse;
mod table;

use aliases::CommandAlias;
pub use error::{CommandParseError, CommandParseErrorKind};
pub(crate) use escape::escape_argument;
use grammar::GrammarParser;
use lexer::Lexer;
use lookup::lookup_command_at;
pub use model::{
    CommandArgument, CommandGrouping, EnvironmentAssignment, ParsedCommand, ParsedCommands,
};
pub use table::{CommandEntry, COMMAND_TABLE};

const DEFAULT_MAX_COMMAND_BYTES: usize = 16 * 1024;
/// Maximum size of a single command parsed from `source-file` input.
pub const SOURCE_FILE_MAX_COMMAND_BYTES: usize = 1024 * 1024;

/// Parses a tmux command string with default expansion context.
pub fn parse_command_string(input: &str) -> Result<ParsedCommands, CommandParseError> {
    CommandParser::new().parse(input)
}

/// Parses a tmux command argument vector with default expansion context.
pub fn parse_command_arguments<I, S>(arguments: I) -> Result<ParsedCommands, CommandParseError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    CommandParser::new().parse_arguments(arguments)
}

/// Returns whether an argv token has the parse-time `name=value` form.
///
/// The identifier grammar is deliberately ASCII and matches tmux's command
/// parser: a letter or underscore followed by letters, digits, or underscores.
#[must_use]
pub fn is_parse_time_assignment(argument: &str) -> bool {
    let Some((name, _)) = argument.split_once('=') else {
        return false;
    };
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Looks up a frozen tmux command using exact alias then unique name prefix.
pub fn lookup_command(name: &str) -> Result<&'static CommandEntry, CommandParseError> {
    lookup_command_at(name, 0, &[])
}

/// A reusable parser with parse-time expansion context.
#[derive(Debug, Clone)]
pub struct CommandParser {
    environment: Vec<(String, String)>,
    format_variables: Vec<(String, String)>,
    home_dir: Option<String>,
    user_home_dirs: Vec<(String, String)>,
    command_aliases: Vec<CommandAlias>,
    exact_commands: &'static [CommandEntry],
    max_command_bytes: usize,
}

impl Default for CommandParser {
    fn default() -> Self {
        Self {
            environment: Vec::new(),
            format_variables: Vec::new(),
            home_dir: None,
            user_home_dirs: Vec::new(),
            command_aliases: Vec::new(),
            exact_commands: &[],
            max_command_bytes: DEFAULT_MAX_COMMAND_BYTES,
        }
    }
}

impl CommandParser {
    /// Creates a parser with no environment, tilde, or user alias overrides.
    #[must_use]
    pub fn new() -> Self {
        let mut parser = Self::default();
        parser.command_aliases.extend(CommandAlias::builtin());
        parser
    }

    /// Adds one variable to the parse-time environment expansion context.
    #[must_use]
    pub fn with_environment_value(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Adds one parse-time format variable used by `%if` condition expansion.
    #[must_use]
    pub fn with_format_value(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.format_variables.push((name.into(), value.into()));
        self
    }

    /// Returns whether parsing `source` can read parse-time format variables.
    ///
    /// Only `%if` and `%elif` conditions expand formats while parsing, and the
    /// lexer recognises a directive only from a literal `%` in the source, so a
    /// source without one never reads what [`Self::with_format_value`] adds.
    #[must_use]
    pub fn source_reads_parse_time_formats(source: &str) -> bool {
        source.contains('%')
    }

    /// Copies global values from an RMUX environment store into the parser.
    #[must_use]
    pub fn with_environment_store(mut self, environment: &EnvironmentStore) -> Self {
        self.environment.extend(
            environment
                .global_entries()
                .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
        self
    }

    /// Adds the fallback home directory used for `~` expansion.
    #[must_use]
    pub fn with_home_dir(mut self, home_dir: impl Into<String>) -> Self {
        self.home_dir = Some(home_dir.into());
        self
    }

    /// Adds a deterministic `~user` expansion mapping.
    #[must_use]
    pub fn with_user_home_dir(
        mut self,
        user: impl Into<String>,
        home_dir: impl Into<String>,
    ) -> Self {
        self.user_home_dirs.push((user.into(), home_dir.into()));
        self
    }

    /// Adds one `command-alias` option entry of the form `name=value`.
    pub fn with_command_alias(
        mut self,
        definition: impl Into<String>,
    ) -> Result<Self, CommandParseError> {
        let definition = definition.into();
        let Some(alias) = CommandAlias::parse(definition) else {
            return Err(CommandParseError::new(
                0,
                "command-alias entry must be name=value",
            ));
        };
        self.command_aliases.push(alias);
        Ok(self)
    }

    /// Replaces the parser alias table with valid `command-alias` entries.
    #[must_use]
    pub fn with_command_aliases<I, S>(mut self, definitions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.command_aliases.clear();
        self.command_aliases
            .extend(definitions.into_iter().filter_map(CommandAlias::parse));
        self
    }

    /// Adds exact-only command names to this parser.
    ///
    /// These entries are intentionally excluded from tmux-style prefix lookup.
    /// Use this for client-side RMUX extensions; server-side `source-file`
    /// parsing should keep the frozen tmux command table.
    #[must_use]
    pub fn with_exact_commands(mut self, commands: &'static [CommandEntry]) -> Self {
        self.exact_commands = commands;
        self
    }

    /// Overrides the maximum parsed command size.
    #[must_use]
    pub fn with_max_command_bytes(mut self, max_command_bytes: usize) -> Self {
        self.max_command_bytes = max_command_bytes;
        self
    }

    /// Parses a tmux command string through the tmux-style lexer.
    pub fn parse(&self, input: &str) -> Result<ParsedCommands, CommandParseError> {
        self.parse_inner(input, false, CommandGrouping::ByLine)
    }

    /// Parses command structure without command-name lookup or alias expansion.
    ///
    /// Source recovery uses this to find the command boundary around a lookup
    /// error without corrupting multi-line brace blocks.
    pub fn parse_structure(&self, input: &str) -> Result<ParsedCommands, CommandParseError> {
        let mut parser = GrammarParser::new(Lexer::new(input, self), CommandGrouping::ByLine);
        let commands = parser.parse_all()?;
        ensure_parsed_command_lengths(&commands, self.max_command_bytes)?;
        Ok(commands)
    }

    /// Parses source-file/startup config text with tmux source-file comment
    /// semantics.
    ///
    /// tmux treats any unquoted `#` outside condition directives as the start of
    /// a comment, even when the next byte is `{`. Command strings parsed from
    /// argv or option values keep RMUX's historical `#{...}` token support; only
    /// source-file text uses this stricter mode.
    pub fn parse_source_file(&self, input: &str) -> Result<ParsedCommands, CommandParseError> {
        self.parse_source_file_inner(input, false, CommandGrouping::ByLine)
    }

    /// Parses source-file structure without command-name lookup or alias
    /// expansion. Recovery uses this to locate command boundaries after a
    /// lookup error while preserving source-file comment semantics.
    pub fn parse_source_file_structure(
        &self,
        input: &str,
    ) -> Result<ParsedCommands, CommandParseError> {
        let mut parser = GrammarParser::new_source_file(
            Lexer::new_source_file(input, self),
            CommandGrouping::ByLine,
        );
        let commands = parser.parse_all()?;
        ensure_parsed_command_lengths(&commands, self.max_command_bytes)?;
        Ok(commands)
    }

    /// Parses a tmux command string with `CMD_PARSE_ONEGROUP` semantics.
    ///
    /// tmux uses this mode when a command string is parsed from an argument or
    /// option value, so embedded newlines do not create independent abort
    /// groups.
    pub fn parse_one_group(&self, input: &str) -> Result<ParsedCommands, CommandParseError> {
        self.parse_inner(input, false, CommandGrouping::OneGroup)
    }

    /// Parses an argv-style tmux command vector.
    ///
    /// tmux treats these arguments as already split and only divides commands
    /// on unescaped trailing semicolons.
    pub fn parse_arguments<I, S>(&self, arguments: I) -> Result<ParsedCommands, CommandParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.parse_arguments_inner(arguments, false)
    }

    /// Parses an argv-style command vector for RMUX's internal runtime bridge.
    ///
    /// Unlike [`Self::parse_arguments`], this recognizes one leading
    /// `name=value` assignment in each command group. The direct argv parser
    /// deliberately keeps tmux's behavior; only the server-owned alias bridge
    /// opts into assignment classification.
    pub fn parse_arguments_with_assignments<I, S>(
        &self,
        arguments: I,
    ) -> Result<ParsedCommands, CommandParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.parse_arguments_inner(arguments, true)
    }

    fn parse_arguments_inner<I, S>(
        &self,
        arguments: I,
        classify_assignments: bool,
    ) -> Result<ParsedCommands, CommandParseError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let arguments = arguments
            .into_iter()
            .map(|argument| argument.as_ref().to_owned())
            .collect::<Vec<_>>();
        let command_bytes = arguments
            .iter()
            .map(String::len)
            .sum::<usize>()
            .saturating_add(arguments.len().saturating_sub(1));
        ensure_command_length(command_bytes, 0, self.max_command_bytes)?;

        let mut commands = ParsedCommands::with_grouping(CommandGrouping::ByLine);
        let mut current = Vec::new();
        let mut group_has_assignment = false;

        for argument in arguments {
            let mut value = argument;
            let mut ends_command = false;

            if value.ends_with(';') {
                value.pop();
                if value.ends_with('\\') {
                    value.pop();
                    value.push(';');
                } else {
                    ends_command = true;
                }
            }

            if !ends_command || !value.is_empty() {
                if classify_assignments
                    && current.is_empty()
                    && !group_has_assignment
                    && is_parse_time_assignment(&value)
                {
                    commands.push_assignment(EnvironmentAssignment::from_equals(value, false));
                    group_has_assignment = true;
                } else {
                    current.push(CommandArgument::String(value));
                }
            }
            if ends_command && !current.is_empty() {
                commands.push_command(command_from_arguments(std::mem::take(&mut current), 1)?);
            }
            if ends_command {
                group_has_assignment = false;
            }
        }

        if !current.is_empty() {
            commands.push_command(command_from_arguments(current, 1)?);
        }

        self.expand_and_lookup(commands, false)
    }

    fn parse_inner(
        &self,
        input: &str,
        no_alias: bool,
        grouping: CommandGrouping,
    ) -> Result<ParsedCommands, CommandParseError> {
        let mut parser = GrammarParser::new(Lexer::new(input, self), grouping);
        let commands = parser.parse_all()?;
        ensure_parsed_command_lengths(&commands, self.max_command_bytes)?;
        self.expand_and_lookup(commands, no_alias)
    }

    fn parse_source_file_inner(
        &self,
        input: &str,
        no_alias: bool,
        grouping: CommandGrouping,
    ) -> Result<ParsedCommands, CommandParseError> {
        let mut parser =
            GrammarParser::new_source_file(Lexer::new_source_file(input, self), grouping);
        let commands = parser.parse_all()?;
        ensure_parsed_command_lengths(&commands, self.max_command_bytes)?;
        self.expand_and_lookup(commands, no_alias)
    }

    fn expand_and_lookup(
        &self,
        commands: ParsedCommands,
        no_alias: bool,
    ) -> Result<ParsedCommands, CommandParseError> {
        let assignments = commands.assignments.clone();
        let mut output = ParsedCommands {
            commands: Vec::new(),
            assignments: commands.assignments,
            grouping: commands.grouping,
        };

        for mut command in commands.commands {
            if !no_alias {
                if let Some(alias) = self.find_command_alias(&command.name) {
                    let mut alias_parser = self.clone();
                    alias_parser.environment.extend(
                        assignments
                            .iter()
                            .map(|assignment| (assignment.name.clone(), assignment.value.clone())),
                    );
                    let mut replacement = alias_parser
                        .parse_inner(alias, true, CommandGrouping::OneGroup)
                        .map_err(|error| error.with_line(command.line))?;
                    for replacement_command in &mut replacement.commands {
                        replacement_command.line = command.line;
                        replacement_command.start_line = command.start_line;
                    }
                    if let Some(last) = replacement.commands.last_mut() {
                        last.arguments.append(&mut command.arguments);
                    }
                    output.append(replacement);
                    continue;
                }
            }

            for argument in &mut command.arguments {
                if let CommandArgument::Commands(nested) = argument {
                    let nested_commands = std::mem::take(nested);
                    *nested = self.expand_and_lookup(nested_commands, no_alias)?;
                }
            }

            let entry = lookup_command_at(&command.name, command.line, self.exact_commands)?;
            command.name = entry.name.to_owned();
            output.push_command(command);
        }

        // Alias arguments are appended after parsing the expansion. Check the
        // completed commands so argv, strings, source files and bindings agree.
        for command in output.commands() {
            argument_check::check_choose_template_arity(command)
                .map_err(|error| CommandParseError::new(command.line(), error.to_string()))?;
        }
        Ok(output)
    }

    fn find_command_alias(&self, name: &str) -> Option<&str> {
        self.command_aliases
            .iter()
            .find(|alias| alias.name() == name)
            .map(CommandAlias::value)
    }

    fn lookup_environment(&self, name: &str) -> Option<&str> {
        self.environment
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.as_str())
    }

    fn expand_tilde(&self, user: &str) -> Option<&str> {
        if user.is_empty() {
            return self
                .lookup_environment("HOME")
                .filter(|home| !home.is_empty())
                .or(self.home_dir.as_deref());
        }

        self.user_home_dirs
            .iter()
            .find(|(candidate, _)| candidate == user)
            .map(|(_, home)| home.as_str())
    }

    fn condition_is_true(&self, value: &str) -> bool {
        let expanded = if value.contains("#{") {
            render_template(
                value,
                &ParseTimeFormatVariables {
                    values: &self.format_variables,
                },
            )
        } else {
            value.to_owned()
        };

        is_truthy(&expanded)
    }
}

fn ensure_command_length(
    bytes: usize,
    line: usize,
    max_command_bytes: usize,
) -> Result<(), CommandParseError> {
    if bytes > max_command_bytes {
        return Err(CommandParseError::new(line, "command too long"));
    }
    Ok(())
}

fn ensure_parsed_command_lengths(
    commands: &ParsedCommands,
    max_command_bytes: usize,
) -> Result<(), CommandParseError> {
    for command in commands.commands() {
        ensure_parsed_command_length(command, max_command_bytes)?;
    }
    Ok(())
}

fn ensure_parsed_command_length(
    command: &ParsedCommand,
    max_command_bytes: usize,
) -> Result<(), CommandParseError> {
    let mut bytes = command.name.len();
    for argument in command.arguments() {
        bytes = bytes.saturating_add(1);
        match argument {
            CommandArgument::String(value) => {
                bytes = bytes.saturating_add(value.len());
            }
            CommandArgument::Commands(commands) => {
                ensure_parsed_command_lengths(commands, max_command_bytes)?;
            }
        }
    }
    ensure_command_length(bytes, command.line(), max_command_bytes)
}

struct ParseTimeFormatVariables<'a> {
    values: &'a [(String, String)],
}

impl FormatVariables for ParseTimeFormatVariables<'_> {
    fn format_value(&self, _variable: FormatVariable) -> Option<String> {
        None
    }

    fn format_value_by_name(&self, name: &str) -> Option<String> {
        self.values
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, value)| value.clone())
    }
}

fn command_from_arguments(
    mut arguments: Vec<CommandArgument>,
    line: usize,
) -> Result<ParsedCommand, CommandParseError> {
    let Some(CommandArgument::String(name)) = arguments.first() else {
        return Err(CommandParseError::new(line, "no command"));
    };
    let name = name.clone();
    arguments.remove(0);
    Ok(ParsedCommand::new(name, arguments, line))
}

#[cfg(test)]
#[path = "command_parser/tests.rs"]
mod tests;
