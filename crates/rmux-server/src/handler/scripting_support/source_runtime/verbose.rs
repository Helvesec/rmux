//! Verbose (`-v`) echo of sourced commands to stdout.

use rmux_core::command_parser::{CommandArgument, ParsedCommand, ParsedCommands};
use rmux_proto::RmuxError;

use super::LoadedSourceFile;

pub(super) fn append_verbose_commands(
    stdout: &mut Vec<u8>,
    current_file: &str,
    parsed: &ParsedCommands,
) {
    if parsed.is_empty() {
        return;
    }
    append_verbose_command_groups(stdout, current_file, parsed.commands(), None);
}

pub(super) fn append_verbose_loaded_commands(
    stdout: &mut Vec<u8>,
    loaded: &LoadedSourceFile,
    stop: Option<&SourceErrorLocation>,
) {
    for batch in &loaded.commands {
        let current_file = batch.current_file.as_deref().unwrap_or_default();
        let stop_line = stop
            .filter(|stop| stop.current_file == current_file)
            .map(|stop| stop.line);
        if append_verbose_command_groups(stdout, current_file, batch.commands.commands(), stop_line)
        {
            return;
        }
    }
}

fn append_verbose_command_groups(
    stdout: &mut Vec<u8>,
    current_file: &str,
    commands: &[ParsedCommand],
    stop_line: Option<usize>,
) -> bool {
    let command_lines = commands.iter().map(ParsedCommand::line).collect::<Vec<_>>();
    let mut current_line = None;
    let mut current_raw_line = None;
    let mut current_commands = Vec::new();

    for (index, command) in commands.iter().enumerate() {
        if stop_line.is_some_and(|stop_line| command.line() >= stop_line) {
            append_verbose_group(stdout, current_file, current_line, &current_commands);
            return true;
        }

        let raw_line = command.line();
        if current_raw_line.is_some_and(|line| line != raw_line) {
            append_verbose_group(stdout, current_file, current_line, &current_commands);
            current_commands.clear();
        }

        for argument in command.arguments() {
            if let CommandArgument::Commands(commands) = argument {
                if append_verbose_command_groups(
                    stdout,
                    current_file,
                    commands.commands(),
                    stop_line,
                ) {
                    return true;
                }
            }
        }

        current_line = Some(verbose_line_for_command(command, &command_lines, index));
        current_raw_line = Some(raw_line);
        current_commands.push(command.to_tmux_string());
    }

    append_verbose_group(stdout, current_file, current_line, &current_commands);
    false
}

fn verbose_line_for_command(
    command: &ParsedCommand,
    command_lines: &[usize],
    index: usize,
) -> usize {
    let raw_line = command.line();
    let has_same_line_neighbor = index
        .checked_sub(1)
        .and_then(|previous| command_lines.get(previous))
        .is_some_and(|line| *line == raw_line)
        || command_lines
            .get(index.saturating_add(1))
            .is_some_and(|line| *line == raw_line);
    if !has_same_line_neighbor {
        if let Some(line) = last_nested_command_line(command) {
            return line;
        }
    }
    raw_line
}

fn last_nested_command_line(command: &ParsedCommand) -> Option<usize> {
    command.arguments().iter().rev().find_map(|argument| {
        let CommandArgument::Commands(commands) = argument else {
            return None;
        };
        last_command_line(commands)
    })
}

fn last_command_line(commands: &ParsedCommands) -> Option<usize> {
    commands
        .commands()
        .last()
        .map(|command| last_nested_command_line(command).unwrap_or_else(|| command.line()))
}

fn append_verbose_group(
    stdout: &mut Vec<u8>,
    current_file: &str,
    line: Option<usize>,
    commands: &[String],
) {
    let Some(line) = line else {
        return;
    };
    stdout.extend_from_slice(current_file.as_bytes());
    stdout.push(b':');
    stdout.extend_from_slice(line.to_string().as_bytes());
    stdout.extend_from_slice(b": ");
    stdout.extend_from_slice(commands.join(" ; ").as_bytes());
    stdout.push(b'\n');
}

pub(super) struct SourceErrorLocation {
    current_file: String,
    line: usize,
}

pub(super) fn source_error_location_for_loaded(
    error: &RmuxError,
    loaded: &LoadedSourceFile,
) -> Option<SourceErrorLocation> {
    let message = error.to_string();
    for line in message.lines() {
        for batch in &loaded.commands {
            let Some(current_file) = batch.current_file.as_deref() else {
                continue;
            };
            let prefix = format!("{current_file}:");
            let Some(start) = line.find(&prefix) else {
                continue;
            };
            let rest = &line[start + prefix.len()..];
            let Some((line_number, _)) = rest.split_once(':') else {
                continue;
            };
            let Ok(line) = line_number.parse::<usize>() else {
                continue;
            };
            return Some(SourceErrorLocation {
                current_file: current_file.to_owned(),
                line,
            });
        }
    }
    None
}
