//! Frozen tmux command lookup helpers.

use super::{CommandEntry, CommandParseError, COMMAND_TABLE};

pub(super) fn lookup_command_at(
    name: &str,
    line: usize,
    exact_commands: &'static [CommandEntry],
) -> Result<&'static CommandEntry, CommandParseError> {
    for entry in COMMAND_TABLE {
        if entry.alias == Some(name) {
            return Ok(entry);
        }
    }
    for entry in exact_commands {
        if entry.alias == Some(name) || entry.name == name {
            return Ok(entry);
        }
    }

    let mut found = None;
    let mut ambiguous = false;
    for entry in COMMAND_TABLE {
        if !entry.name.starts_with(name) {
            continue;
        }
        if found.is_some() {
            ambiguous = true;
        }
        found = Some(entry);
        if entry.name == name {
            ambiguous = false;
            break;
        }
    }

    if ambiguous {
        let candidates = COMMAND_TABLE
            .iter()
            .filter(|entry| entry.name.starts_with(name))
            .map(|entry| entry.name)
            .collect::<Vec<_>>()
            .join(", ");
        return Err(CommandParseError::new(
            line,
            format!("ambiguous command: {name}, could be: {candidates}"),
        ));
    }

    found.ok_or_else(|| CommandParseError::new(line, format!("unknown command: {name}")))
}
