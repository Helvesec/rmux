//! Errors returned by command tokenization, parsing, or lookup.

use std::error::Error;
use std::fmt;

/// Error returned by command tokenization, parsing, or lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandParseError {
    line: usize,
    message: String,
    kind: CommandParseErrorKind,
    unknown_command: Option<String>,
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
            unknown_command: None,
        }
    }

    pub(crate) fn structural(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            kind: CommandParseErrorKind::Structural,
            unknown_command: None,
        }
    }

    /// Command name retained by a failed lookup, without inspecting diagnostic text.
    #[must_use]
    pub fn unknown_command(&self) -> Option<&str> {
        self.unknown_command.as_deref()
    }

    pub(crate) fn unknown(line: usize, name: &str) -> Self {
        let mut error = Self::lookup(line, format!("unknown command: {name}"));
        error.unknown_command = Some(name.to_owned());
        error
    }

    pub(crate) fn lookup(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
            kind: CommandParseErrorKind::Lookup,
            unknown_command: None,
        }
    }

    pub(super) fn with_line(mut self, line: usize) -> Self {
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
