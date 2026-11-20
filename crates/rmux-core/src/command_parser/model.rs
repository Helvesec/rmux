//! Parser output: parsed command lists, commands, arguments and parse-time
//! environment assignments, with their conversion back to tmux syntax.

use super::escape::escape_argument;
use super::reparse::escape_argument_for_reparse;

/// Parser output for one command list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCommands {
    pub(super) commands: Vec<ParsedCommand>,
    pub(super) assignments: Vec<EnvironmentAssignment>,
    pub(super) grouping: CommandGrouping,
}

impl ParsedCommands {
    pub(super) fn with_grouping(grouping: CommandGrouping) -> Self {
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

    pub(super) fn push_assignment(&mut self, assignment: EnvironmentAssignment) {
        self.assignments.push(assignment);
    }

    pub(super) fn push_command(&mut self, mut command: ParsedCommand) {
        command.drain_nested_assignments_into(&mut self.assignments);
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
            .map(ParsedCommand::to_tmux_reparse_string)
            .collect::<Vec<_>>()
            .join(" \\; ")
    }

    /// Converts the parsed commands to a lossless command string that can be
    /// parsed again without applying display-only quote escaping twice.
    #[must_use]
    pub fn to_tmux_reparse_string(&self) -> String {
        let mut rendered = self.assignments_to_tmux_reparse_string();
        let mut previous_line = None;
        for command in &self.commands {
            if !rendered.is_empty() {
                if previous_line.is_some_and(|line| line != command.line()) {
                    rendered.push_str(" ;; ");
                } else {
                    rendered.push_str(" ; ");
                }
            }
            rendered.push_str(&command.to_tmux_reparse_string());
            previous_line = Some(command.line());
        }
        rendered
    }

    /// Converts only the parse-time assignments to a lossless command string.
    #[must_use]
    pub fn assignments_to_tmux_reparse_string(&self) -> String {
        self.assignments
            .iter()
            .map(|assignment| {
                let rendered = escape_argument_for_reparse(&format!(
                    "{}={}",
                    assignment.name(),
                    assignment.value()
                ));
                if assignment.hidden() {
                    format!("%hidden {rendered}")
                } else {
                    rendered
                }
            })
            .collect::<Vec<_>>()
            .join(" ; ")
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
    pub(super) name: String,
    pub(super) arguments: Vec<CommandArgument>,
    pub(super) start_line: usize,
    pub(super) line: usize,
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

    /// Returns this command with replacement arguments while preserving its
    /// source location.
    #[must_use]
    pub fn with_arguments(mut self, arguments: Vec<CommandArgument>) -> Self {
        self.arguments = arguments;
        self
    }

    /// Returns the one-based input line where this command started.
    #[must_use]
    pub fn line(&self) -> usize {
        self.line
    }

    pub(super) fn new(name: String, arguments: Vec<CommandArgument>, line: usize) -> Self {
        Self {
            name,
            arguments,
            start_line: line,
            line,
        }
    }

    pub(super) fn with_lines(
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

    fn drain_nested_assignments_into(&mut self, assignments: &mut Vec<EnvironmentAssignment>) {
        for argument in &mut self.arguments {
            if let CommandArgument::Commands(commands) = argument {
                assignments.append(&mut commands.assignments);
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

    /// Converts this command to a lossless string for an internal
    /// parse-execute bridge.
    #[must_use]
    pub fn to_tmux_reparse_string(&self) -> String {
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
    pub(super) name: String,
    pub(super) value: String,
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

    pub(super) fn from_equals(value: String, hidden: bool) -> Self {
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
