use std::ffi::{OsStr, OsString};

use rmux_core::command_parser::{
    CommandArgument, CommandParseError, CommandParser as TmuxCommandParser, ParsedCommand,
    ParsedCommands,
};

use super::*;

pub(super) mod normalization;
pub(crate) mod schema;

pub(crate) fn parse_command_queue(arguments: &[OsString]) -> Result<ParsedCommands, clap::Error> {
    if arguments.is_empty() {
        return Ok(ParsedCommands::default());
    }

    let arguments = arguments
        .iter()
        .map(|argument| command_argument_to_string(argument))
        .collect::<Result<Vec<_>, _>>()?;
    let arguments = expand_cli_argument_aliases(arguments);
    TmuxCommandParser::new()
        .with_exact_commands(super::RMUX_EXTENSION_COMMANDS)
        .parse_arguments(&arguments)
        .map_err(command_parse_error_to_clap)
}

pub(super) fn parse_runtime_command_groups(
    groups: &[RuntimeCommandGroup],
) -> Result<ParsedCommands, clap::Error> {
    let parser = TmuxCommandParser::new()
        .with_command_aliases(std::iter::empty::<String>())
        .with_exact_commands(super::RMUX_EXTENSION_COMMANDS);
    let mut parsed = ParsedCommands::default();

    for group in groups {
        let RuntimeCommandGroup::Canonical(rendered) = group;
        if rendered.is_empty() {
            continue;
        }
        let commands = parser
            .parse_one_group(rendered)
            .map_err(command_parse_error_to_clap)?;
        parsed.append(commands);
    }

    Ok(parsed)
}

fn expand_cli_argument_aliases(arguments: Vec<String>) -> Vec<String> {
    let mut expanded = Vec::with_capacity(arguments.len() + 2);
    let mut command_start = true;

    for argument in arguments {
        let (base, ends_command) = split_cli_command_terminator(&argument);
        if command_start {
            if let Some(alias) = documented_cli_aliases()
                .iter()
                .find(|alias| alias.alias == base)
            {
                let mut words = alias.expansion.split_ascii_whitespace().peekable();
                while let Some(word) = words.next() {
                    expanded.push(if ends_command && words.peek().is_none() {
                        format!("{word};")
                    } else {
                        word.to_owned()
                    });
                }
                command_start = ends_command;
                continue;
            }
        }

        expanded.push(argument);
        command_start = ends_command;
    }

    expanded
}

fn split_cli_command_terminator(argument: &str) -> (&str, bool) {
    if let Some(stripped) = argument.strip_suffix(';') {
        if stripped.ends_with('\\') {
            (argument, false)
        } else {
            (stripped, true)
        }
    } else {
        (argument, false)
    }
}

fn command_argument_to_string(argument: &OsStr) -> Result<String, clap::Error> {
    argument.to_str().map(str::to_owned).ok_or_else(|| {
        clap::Error::raw(
            clap::error::ErrorKind::InvalidUtf8,
            "invalid UTF-8 in command argument",
        )
    })
}

fn command_parse_error_to_clap(error: CommandParseError) -> clap::Error {
    let message = cli_command_error_message(error.message());

    let kind =
        if message.starts_with("unknown command: ") || message.starts_with("ambiguous command: ") {
            clap::error::ErrorKind::InvalidSubcommand
        } else {
            clap::error::ErrorKind::ValueValidation
        };
    clap::Error::raw(kind, message.to_owned())
}

fn cli_command_error_message(message: &str) -> &str {
    let original = message;
    let Some(rest) = original.strip_prefix("-:") else {
        return message;
    };
    let Some((line, stripped)) = rest.split_once(": ") else {
        return original;
    };
    if line.bytes().all(|byte| byte.is_ascii_digit()) {
        stripped
    } else {
        original
    }
}

pub(super) fn command_from_parsed(command: ParsedCommand) -> Result<Command, clap::Error> {
    let name = command.name().to_owned();
    let error_command_name = name.clone();
    let queue_command = command.to_tmux_reparse_string();
    let arguments = command_arguments_for_clap(command.arguments());
    let parsed = super::schema::parse_command(name, arguments, queue_command);

    parsed.map_err(|mut error| {
        error.insert(
            clap::error::ContextKind::Custom,
            clap::error::ContextValue::String(error_command_name),
        );
        error
    })
}

pub(crate) fn command_arguments_for_clap(arguments: &[CommandArgument]) -> Vec<String> {
    arguments
        .iter()
        .map(|argument| match argument {
            CommandArgument::String(value) => value.clone(),
            CommandArgument::Commands(_) => argument.to_tmux_reparse_string(),
        })
        .collect()
}

pub(super) fn parse_send_keys_args(arguments: Vec<String>) -> Result<SendKeysArgs, clap::Error> {
    let has_wait = arguments.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--wait"
                | "--wait-text"
                | "--wait-visible-text"
                | "--wait-next-text"
                | "--wait-pane-exit"
        ) || argument.starts_with("--wait=")
            || argument.starts_with("--wait-text=")
            || argument.starts_with("--wait-visible-text=")
            || argument.starts_with("--wait-next-text=")
    });
    if has_wait && !arguments.iter().any(|argument| argument == "--") {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "command send-keys: -- is required before payload when using --wait options",
        ));
    }
    parse_command_args::<SendKeysArgs>("send-keys", arguments).and_then(SendKeysArgs::validate)
}

pub(super) fn parse_server_access_args(
    arguments: Vec<String>,
) -> Result<ServerAccessArgs, clap::Error> {
    for argument in &arguments {
        if argument == "--" {
            break;
        }
        if argument == "--help" {
            continue;
        }
        if argument.starts_with("--") {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                "command server-access: invalid flag --",
            ));
        }
        if argument == "-" {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::UnknownArgument,
                "command server-access: invalid flag -",
            ));
        }
        let Some(flags) = argument.strip_prefix('-') else {
            continue;
        };
        if flags.is_empty() {
            continue;
        }
        for flag in flags.chars() {
            if flag == 't' {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::UnknownArgument,
                    "command server-access: unknown flag -t",
                ));
            }
            if !matches!(flag, 'a' | 'd' | 'l' | 'r' | 'w') {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::UnknownArgument,
                    format!("command server-access: unknown flag -{flag}"),
                ));
            }
        }
    }

    let args = parse_command_args::<ServerAccessArgs>("server-access", arguments)?;
    args.validate()
}

pub(super) fn parse_set_option_args<T>(
    command_name: &'static str,
    mut arguments: Vec<String>,
    kind: SetOptionCommandKind,
) -> Result<SetOptionArgs, clap::Error>
where
    T: Args + FromArgMatches + Into<SetOptionArgs>,
{
    let trailing_literal_separator = normalize_set_option_separator(command_name, &mut arguments)?;
    let explicit_scope = matches!(kind, SetOptionCommandKind::SetOption)
        .then(|| set_option_scope(&arguments))
        .flatten();
    let mut args = parse_command_args::<T>(command_name, arguments)?.into();
    apply_set_option_scope(&mut args, explicit_scope);
    if trailing_literal_separator {
        if args.value.is_some() {
            return Err(set_option_too_many_arguments(command_name));
        }
        args.value = Some("--".to_owned());
    }
    args.validate(kind)
}

#[derive(Debug, Clone, Copy)]
enum SetOptionScopeFlag {
    Server,
    Window,
    Pane,
}

#[derive(Debug, Default, Clone, Copy)]
struct SetOptionScopeFlags {
    server: bool,
    window: bool,
    pane: bool,
}

impl SetOptionScopeFlags {
    const fn selected(self) -> Option<SetOptionScopeFlag> {
        if self.server {
            Some(SetOptionScopeFlag::Server)
        } else if self.pane {
            Some(SetOptionScopeFlag::Pane)
        } else if self.window {
            Some(SetOptionScopeFlag::Window)
        } else {
            None
        }
    }
}

fn set_option_scope(arguments: &[String]) -> Option<SetOptionScopeFlag> {
    let mut scopes = SetOptionScopeFlags::default();
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--" {
            break;
        }
        if !argument.starts_with('-') || argument == "-" {
            break;
        }
        if argument.starts_with("-t") && argument.len() > 2 {
            index += 1;
            continue;
        }

        let mut chars = argument[1..].chars().peekable();
        while let Some(flag) = chars.next() {
            match flag {
                's' => scopes.server = true,
                // -U is an unset modifier, not a scope selector: plain
                // `set -U` targets the session copy (oracle 2026-07-09) and
                // -p/-w keep their own precedence when combined with it.
                'w' => scopes.window = true,
                'p' => scopes.pane = true,
                't' => {
                    if chars.peek().is_none() {
                        index += 1;
                    }
                    break;
                }
                _ => {}
            }
        }
        index += 1;
    }

    scopes.selected()
}

fn apply_set_option_scope(args: &mut SetOptionArgs, scope: Option<SetOptionScopeFlag>) {
    let Some(scope) = scope else {
        return;
    };
    args.server = false;
    args.window = false;
    args.pane = false;
    match scope {
        SetOptionScopeFlag::Server => args.server = true,
        SetOptionScopeFlag::Window => args.window = true,
        SetOptionScopeFlag::Pane => args.pane = true,
    }
}

fn normalize_set_option_separator(
    command_name: &'static str,
    arguments: &mut Vec<String>,
) -> Result<bool, clap::Error> {
    let Some(index) = arguments.iter().position(|argument| argument == "--") else {
        return Ok(false);
    };
    if index + 1 == arguments.len() {
        let _ = arguments.pop();
        return Ok(true);
    }
    if set_option_positionals_before_separator(&arguments[..index]) > 0 {
        return Err(set_option_too_many_arguments(command_name));
    }
    let _ = arguments.remove(index);
    Ok(false)
}

fn set_option_positionals_before_separator(arguments: &[String]) -> usize {
    let mut positionals = 0;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "-t" {
            index += 2;
            continue;
        }
        if argument.starts_with("-t") && argument.len() > 2 {
            index += 1;
            continue;
        }
        if argument.starts_with('-') && argument.len() > 1 {
            let flags = &argument[1..];
            if let Some((offset, flag)) = flags.char_indices().find(|(_, flag)| *flag == 't') {
                index += if offset + flag.len_utf8() == flags.len() {
                    2
                } else {
                    1
                };
                continue;
            }
            index += 1;
            continue;
        }
        positionals += 1;
        index += 1;
    }
    positionals
}

fn set_option_too_many_arguments(command_name: &'static str) -> clap::Error {
    clap::Error::raw(
        clap::error::ErrorKind::TooManyValues,
        format!("command {command_name}: too many arguments (need at most 2)"),
    )
}

pub(super) fn parse_set_buffer_args(arguments: Vec<String>) -> Result<SetBufferArgs, clap::Error> {
    let args = parse_command_args::<SetBufferArgs>("set-buffer", arguments)?;
    args.validate()
}

pub(super) fn with_queue_command<T: QueuedCommand>(mut args: T, queue_command: String) -> T {
    args.set_queue_command(queue_command);
    args
}
