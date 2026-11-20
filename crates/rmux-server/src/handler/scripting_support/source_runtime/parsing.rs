use rmux_core::command_parser::{CommandParseErrorKind, CommandParser, ParsedCommand};

use super::super::source_files::{source_parse_error_with_line_offset, SourceInput};
use super::{ParsedSourceInput, RmuxError};

const SOURCE_PARSE_RECOVERY_ERROR_LIMIT: usize = 256;

pub(super) fn parse_source_fragment_recovering_for_import_compat(
    parser: &CommandParser,
    input: &SourceInput,
    contents: &str,
    line_offset: usize,
    parsed: &mut ParsedSourceInput,
) {
    let mut fragments = vec![(contents, line_offset)];
    while let Some((fragment, fragment_line_offset)) = fragments.pop() {
        if fragment.trim().is_empty() {
            continue;
        }
        if parsed.errors.len() >= SOURCE_PARSE_RECOVERY_ERROR_LIMIT {
            parsed.errors.push(RmuxError::Server(format!(
                "{}: too many config parse errors; stopped recovery after {SOURCE_PARSE_RECOVERY_ERROR_LIMIT} errors",
                input.current_file
            )));
            break;
        }

        match parser.parse_source_file(fragment) {
            Ok(mut commands) => {
                if !commands.is_empty() || !commands.assignments().is_empty() {
                    commands.add_line_offset(fragment_line_offset);
                    parsed.commands.push(commands);
                }
            }
            Err(error) => {
                let error_line = error.line();
                let recoverable = error.kind() == CommandParseErrorKind::Lookup;
                parsed.errors.push(source_parse_error_with_line_offset(
                    input,
                    error,
                    fragment_line_offset,
                ));
                if !recoverable {
                    parsed.has_fatal_parse_error = true;
                    continue;
                }
                let Some((prefix, suffix, suffix_line)) =
                    split_around_source_command(parser, fragment, error_line)
                        .or_else(|| split_around_source_line(fragment, error_line))
                else {
                    continue;
                };
                if !suffix.trim().is_empty() && suffix.len() < fragment.len() {
                    fragments.push((
                        suffix,
                        fragment_line_offset.saturating_add(suffix_line.saturating_sub(1)),
                    ));
                }
                if !prefix.trim().is_empty() && prefix.len() < fragment.len() {
                    fragments.push((prefix, fragment_line_offset));
                }
            }
        }
    }
}

pub(super) fn parse_source_fragment_for_execution(
    parser: &CommandParser,
    input: &SourceInput,
    contents: &str,
    line_offset: usize,
    parsed: &mut ParsedSourceInput,
) {
    parse_source_fragment_until_first_error(parser, input, contents, line_offset, parsed);
    if !parsed.errors.is_empty() {
        parsed.has_fatal_parse_error = true;
    }
}

pub(super) fn parse_source_fragment_until_first_error(
    parser: &CommandParser,
    input: &SourceInput,
    contents: &str,
    line_offset: usize,
    parsed: &mut ParsedSourceInput,
) {
    if contents.trim().is_empty() || !parsed.errors.is_empty() {
        return;
    }

    match parser.parse_source_file(contents) {
        Ok(mut commands) => {
            if !commands.is_empty() || !commands.assignments().is_empty() {
                commands.add_line_offset(line_offset);
                parsed.commands.push(commands);
            }
        }
        Err(error) => {
            let error_line = error.line();
            let recoverable = error.kind() == CommandParseErrorKind::Lookup;
            let contextual_error = source_parse_error_with_line_offset(input, error, line_offset);
            if recoverable {
                if let Some((prefix, _, _)) =
                    split_around_source_command(parser, contents, error_line)
                        .or_else(|| split_around_source_line(contents, error_line))
                {
                    if !prefix.trim().is_empty() && prefix.len() < contents.len() {
                        parse_source_fragment_until_first_error(
                            parser,
                            input,
                            prefix,
                            line_offset,
                            parsed,
                        );
                        if !parsed.errors.is_empty() {
                            return;
                        }
                    }
                }
            }
            parsed.errors.push(contextual_error);
        }
    }
}

fn split_around_source_command<'a>(
    parser: &CommandParser,
    contents: &'a str,
    line: usize,
) -> Option<(&'a str, &'a str, usize)> {
    if line == 0 {
        return None;
    }
    let structure = parser.parse_source_file_structure(contents).ok()?;
    let commands = structure.commands();
    let (index, command) = commands.iter().enumerate().find(|(index, command)| {
        let start_line = command.start_line();
        let next_line = commands
            .get(index.saturating_add(1))
            .map(ParsedCommand::start_line)
            .unwrap_or_else(|| {
                contents
                    .lines()
                    .count()
                    .saturating_add(1)
                    .max(start_line + 1)
            });
        start_line <= line && line < next_line
    })?;
    let start = line_start_byte(contents, command.start_line())?;
    let end_line = commands
        .get(index.saturating_add(1))
        .map(ParsedCommand::start_line)
        .unwrap_or_else(|| contents.lines().count().saturating_add(1));
    let end = line_start_byte(contents, end_line).unwrap_or(contents.len());
    if start >= end || start > contents.len() || end > contents.len() {
        return None;
    }
    Some((&contents[..start], &contents[end..], end_line))
}

fn split_around_source_line(contents: &str, line: usize) -> Option<(&str, &str, usize)> {
    if line == 0 {
        return None;
    }
    let last_line = contents.lines().count().max(1);
    let mut start =
        line_start_byte(contents, line).or_else(|| line_start_byte(contents, last_line))?;
    if start == contents.len() && line > 1 {
        start = line_start_byte(contents, last_line)?;
    }
    let next = line_start_byte(contents, line.saturating_add(1)).unwrap_or(contents.len());
    if start == contents.len() && next == contents.len() {
        return None;
    }
    Some((
        &contents[..start],
        &contents[next..],
        line.saturating_add(1),
    ))
}

fn line_start_byte(contents: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }

    let mut current_line = 1usize;
    for (index, byte) in contents.bytes().enumerate() {
        if byte == b'\n' {
            current_line += 1;
            if current_line == line {
                return Some(index + 1);
            }
        }
    }
    (current_line == line).then_some(contents.len())
}
