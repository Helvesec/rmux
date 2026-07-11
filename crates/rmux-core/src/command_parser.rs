//! tmux-compatible command tokenization and command-name lookup.
//!
//! This module mirrors the frozen tmux `cmd-parse.y` lexer boundary closely
//! enough for RMUX command dispatch and config parsing to share one parser.
//! Frozen source anchors: `/opt/rmux/reference/tmux` at commit
//! `31d77e29b6c9fbb07d032018da78db3a8a38d979`, especially `cmd.c:121`
//! (`cmd_table[]`) and `cmd-parse.y:1053`, `cmd-parse.y:1201`,
//! `cmd-parse.y:1626` for argv parsing, continuation handling, and
//! tokenization.

use std::error::Error;
use std::fmt;

use crate::{
    formats::{is_truthy, render_template, FormatVariable, FormatVariables},
    EnvironmentStore,
};

#[path = "command_parser/aliases.rs"]
mod aliases;
#[path = "command_parser/grammar.rs"]
mod grammar;
#[path = "command_parser/lexer.rs"]
mod lexer;
#[path = "command_parser/lookup.rs"]
mod lookup;
#[path = "command_parser/table.rs"]
mod table;

use aliases::CommandAlias;
use grammar::GrammarParser;
use lexer::Lexer;
use lookup::lookup_command_at;
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

/// Looks up a frozen tmux command using exact alias then unique name prefix.
pub fn lookup_command(name: &str) -> Result<&'static CommandEntry, CommandParseError> {
    lookup_command_at(name, 0, &[])
}

/// Parser output for one command list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommands {
    commands: Vec<ParsedCommand>,
    assignments: Vec<EnvironmentAssignment>,
    grouping: CommandGrouping,
}

impl ParsedCommands {
    fn with_grouping(grouping: CommandGrouping) -> Self {
        Self {
            commands: Vec::new(),
            assignments: Vec::new(),
            grouping,
        }
    }

    /// Returns the parsed command sequence.
    #[must_use]
    pub fn commands(&self) -> &[ParsedCommand] {
        &self.commands
    }

    /// Returns parse-time environment assignments.
    #[must_use]
    pub fn assignments(&self) -> &[EnvironmentAssignment] {
        &self.assignments
    }

    /// Returns how queue group IDs should be assigned for this parsed list.
    #[must_use]
    pub const fn grouping(&self) -> CommandGrouping {
        self.grouping
    }

    /// Consumes the list and returns only the command sequence.
    #[must_use]
    pub fn into_commands(self) -> Vec<ParsedCommand> {
        self.commands
    }

    /// Returns whether the parser found no executable commands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    fn push_assignment(&mut self, assignment: EnvironmentAssignment) {
        self.assignments.push(assignment);
    }

    fn push_command(&mut self, command: ParsedCommand) {
        self.commands.push(command);
    }

    /// Appends another parsed command list, preserving the grouping mode of
    /// this list.
    pub fn append(&mut self, mut other: Self) {
        self.assignments.append(&mut other.assignments);
        self.commands.append(&mut other.commands);
    }

    /// Adds an offset to every source line recorded in this command list.
    ///
    /// Recovery parsers use this after parsing a suffix of a larger source
    /// file so diagnostics and verbose output still reference original lines.
    pub fn add_line_offset(&mut self, offset: usize) {
        if offset == 0 {
            return;
        }
        for command in &mut self.commands {
            command.add_line_offset(offset);
        }
    }

    /// Converts the parsed commands back to a tmux-style command string.
    #[must_use]
    pub fn to_tmux_string(&self) -> String {
        let mut rendered = String::new();
        let mut previous_line = None;
        for command in &self.commands {
            if !rendered.is_empty() {
                if previous_line.is_some_and(|line| line != command.line()) {
                    rendered.push_str(" ;; ");
                } else {
                    rendered.push_str(" ; ");
                }
            }
            rendered.push_str(&command.to_tmux_string());
            previous_line = Some(command.line());
        }
        rendered
    }

    /// Converts the parsed commands to a command string suitable for
    /// embedding in a `bind-key` line.
    #[must_use]
    pub fn to_tmux_binding_string(&self) -> String {
        self.commands
            .iter()
            .map(ParsedCommand::to_reparse_string)
            .collect::<Vec<_>>()
            .join(" \\; ")
    }

    /// Converts the parsed commands to a lossless command string that can be
    /// parsed again without applying display-only quote escaping twice.
    #[must_use]
    pub fn to_tmux_reparse_string(&self) -> String {
        let mut rendered = String::new();
        let mut previous_line = None;
        for command in &self.commands {
            if !rendered.is_empty() {
                if previous_line.is_some_and(|line| line != command.line()) {
                    rendered.push_str(" ;; ");
                } else {
                    rendered.push_str(" ; ");
                }
            }
            rendered.push_str(&command.to_reparse_string());
            previous_line = Some(command.line());
        }
        rendered
    }
}

/// Queue grouping mode captured while parsing a tmux command list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandGrouping {
    /// Commands that start on the same source line share one queue group.
    #[default]
    ByLine,
    /// All commands in the parsed list share one queue group.
    OneGroup,
}

/// One parsed tmux command with a canonical command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    name: String,
    arguments: Vec<CommandArgument>,
    start_line: usize,
    line: usize,
}

impl ParsedCommand {
    /// Returns the canonical command name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the parsed command arguments.
    #[must_use]
    pub fn arguments(&self) -> &[CommandArgument] {
        &self.arguments
    }

    /// Returns the one-based input line where this command started.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    fn new(name: String, arguments: Vec<CommandArgument>, line: usize) -> Self {
        Self {
            name,
            arguments,
            start_line: line,
            line,
        }
    }

    fn with_lines(
        name: String,
        arguments: Vec<CommandArgument>,
        start_line: usize,
        line: usize,
    ) -> Self {
        Self {
            name,
            arguments,
            start_line,
            line,
        }
    }

    fn add_line_offset(&mut self, offset: usize) {
        self.start_line = self.start_line.saturating_add(offset);
        self.line = self.line.saturating_add(offset);
        for argument in &mut self.arguments {
            if let CommandArgument::Commands(commands) = argument {
                commands.add_line_offset(offset);
            }
        }
    }

    /// Returns the first one-based input line occupied by this command.
    #[must_use]
    pub fn start_line(&self) -> usize {
        self.start_line
    }

    /// Converts this command back to a tmux-style command string.
    #[must_use]
    pub fn to_tmux_string(&self) -> String {
        std::iter::once(self.name.clone())
            .chain(self.arguments.iter().map(CommandArgument::to_tmux_string))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn to_reparse_string(&self) -> String {
        std::iter::once(self.name.clone())
            .chain(
                self.arguments
                    .iter()
                    .map(CommandArgument::to_reparse_string),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A parsed command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandArgument {
    /// A scalar string argument after tmux quote and expansion handling.
    String(String),
    /// A brace-delimited nested command list.
    Commands(ParsedCommands),
}

impl CommandArgument {
    /// Returns the string value when this is a scalar argument.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Commands(_) => None,
        }
    }

    /// Converts the argument to a string suitable for the legacy CLI bridge.
    #[must_use]
    pub fn to_tmux_string(&self) -> String {
        match self {
            Self::String(value) => escape_argument(value),
            Self::Commands(commands) => format!("{{ {} }}", commands.to_tmux_string()),
        }
    }

    fn to_reparse_string(&self) -> String {
        match self {
            Self::String(value) => escape_argument_for_reparse(value),
            Self::Commands(commands) => {
                format!("{{ {} }}", commands.to_tmux_reparse_string())
            }
        }
    }

    /// Converts this argument to a lossless representation for an internal
    /// parse-execute bridge.
    #[must_use]
    pub fn to_tmux_reparse_string(&self) -> String {
        self.to_reparse_string()
    }
}

/// A parse-time `name=value` environment assignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentAssignment {
    name: String,
    value: String,
    hidden: bool,
}

impl EnvironmentAssignment {
    /// Returns the assignment variable name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the assignment value.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Returns whether `%hidden` preceded this assignment.
    #[must_use]
    pub fn hidden(&self) -> bool {
        self.hidden
    }

    fn from_equals(value: String, hidden: bool) -> Self {
        let (name, value) = value
            .split_once('=')
            .expect("lexer only classifies assignments containing '='");
        Self {
            name: name.to_owned(),
            value: value.to_owned(),
            hidden,
        }
    }
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
                current.push(CommandArgument::String(value));
            }
            if ends_command && !current.is_empty() {
                commands.push_command(command_from_arguments(std::mem::take(&mut current), 1)?);
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

        Ok(output)
    }

    fn find_command_alias(&self, name: &str) -> Option<&str> {
        self.command_aliases
            .iter()
            .rev()
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

/// Error returned by command tokenization, parsing, or lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandParseError {
    line: usize,
    message: String,
    kind: CommandParseErrorKind,
}

/// Coarse parse error class used by source-file recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandParseErrorKind {
    /// The parser cannot safely identify a complete command boundary.
    Structural,
    /// Command name lookup failed after a structurally valid parse.
    Lookup,
    /// Tokenization or command-size validation failed.
    Other,
}

impl CommandParseError {
    /// Returns the one-based input line for the error, or zero when unknown.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    /// Returns the tmux-style error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the coarse error class.
    #[must_use]
    pub const fn kind(&self) -> CommandParseErrorKind {
        self.kind
    }

    pub(crate) fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            kind: CommandParseErrorKind::Other,
        }
    }

    pub(crate) fn structural(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            kind: CommandParseErrorKind::Structural,
        }
    }

    pub(crate) fn lookup(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            kind: CommandParseErrorKind::Lookup,
        }
    }

    fn with_line(mut self, line: usize) -> Self {
        self.line = line;
        self
    }
}

impl fmt::Display for CommandParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for CommandParseError {}

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

pub(crate) fn escape_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_owned();
    }
    if is_single_char_escaped_argument(value) {
        return escape_unquoted_argument(value);
    }
    if !value.chars().any(argument_needs_double_quotes) {
        if value.contains('"') {
            return format!("'{}'", escape_single_quoted_argument(value));
        }
        return escape_unquoted_argument(value);
    }

    format!("\"{}\"", escape_double_quoted_argument(value))
}

fn escape_argument_for_reparse(value: &str) -> String {
    let single_quoted_display =
        value.contains('"') && !value.chars().any(argument_needs_double_quotes);
    if single_quoted_display && value.chars().any(|ch| ch == '\\' || ch.is_ascii_control()) {
        return format!("\"{}\"", escape_double_quoted_argument(value));
    }
    escape_argument(value)
}

fn is_single_char_escaped_argument(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(ch) = chars.next() else {
        return false;
    };
    chars.next().is_none() && matches!(ch, ';' | '{' | '}' | '\'' | '"' | '#' | '$' | '%')
}

fn argument_needs_double_quotes(ch: char) -> bool {
    matches!(ch, ' ' | ';' | '{' | '}' | '\'' | '#' | '$' | '%')
        || (ch.is_whitespace() && !matches!(ch, '\n' | '\r' | '\t'))
}

fn escape_unquoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for (index, ch) in value.chars().enumerate() {
        match ch {
            '~' if index == 0 => escaped.push_str(r"\~"),
            ';' | '{' | '}' | '\'' | '"' | '#' | '$' | '%' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\\' => escaped.push_str(r"\\"),
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

fn escape_single_quoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\\' => escaped.push_str(r"\\"),
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

fn escape_double_quoted_argument(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    let mut chars = value.chars().enumerate().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '~' if index == 0 => escaped.push_str(r"\~"),
            '\n' => escaped.push_str(r"\n"),
            '\r' => escaped.push_str(r"\r"),
            '\t' => escaped.push_str(r"\t"),
            '\u{7}' => escaped.push_str(r"\a"),
            '\u{8}' => escaped.push_str(r"\b"),
            '\u{b}' => escaped.push_str(r"\v"),
            '\u{c}' => escaped.push_str(r"\f"),
            '\u{1b}' => escaped.push_str(r"\033"),
            '$' if chars
                .peek()
                .is_some_and(|(_, next)| dollar_starts_variable(*next)) =>
            {
                escaped.push_str(r"\$")
            }
            '\\' | '"' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escape_control_argument_char(&mut escaped, ch),
        }
    }
    escaped
}

fn escape_control_argument_char(escaped: &mut String, ch: char) {
    match ch {
        '\u{7}' => escaped.push_str(r"\a"),
        '\u{8}' => escaped.push_str(r"\b"),
        '\u{b}' => escaped.push_str(r"\v"),
        '\u{c}' => escaped.push_str(r"\f"),
        '\u{1b}' => escaped.push_str(r"\033"),
        '\0'..='\u{1f}' | '\u{7f}' => {
            escaped.push('\\');
            escaped.push_str(&format!("{:03o}", ch as u32));
        }
        _ => escaped.push(ch),
    }
}

fn dollar_starts_variable(ch: char) -> bool {
    ch == '{' || ch == '_' || ch.is_ascii_alphabetic()
}

#[cfg(test)]
#[path = "command_parser/tests.rs"]
mod tests;
