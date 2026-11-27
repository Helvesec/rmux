//! Typed projections of the shared public command declaration.

use super::*;
use super::{
    parse_send_keys_args, parse_server_access_args, parse_set_buffer_args, parse_set_option_args,
    with_queue_command,
};

#[derive(Debug, Clone, Args)]
struct EmptyArgs {}

/// A single typed command projection, usable without linking every CLI command.
pub(crate) trait CommandSpec {
    type Arguments;
    const NAME: &'static str;
    fn help() -> clap::Command;
    fn parse(arguments: Vec<String>, queue: String) -> Result<Self::Arguments, clap::Error>;
}

macro_rules! argument_type {
    () => {
        ()
    };
    ($args:ty) => {
        $args
    };
}

macro_rules! wrap_command {
    ($variant:ident(), $args:expr) => {{
        let () = $args;
        Command::$variant
    }};
    ($variant:ident($ty:ty), $args:expr) => {
        Command::$variant($args)
    };
}

macro_rules! parse_policy {
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $($args:ty)?] empty) => {
        parse_command_args::<$help>($name, $arguments).map(|_| ())
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] plain) => {
        parse_command_args::<$args>($name, $arguments)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] converted) => {
        parse_command_args::<$help>($name, $arguments).map(<$args>::from)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] set_option $kind:path) => {
        parse_set_option_args::<$help>($name, $arguments, $kind)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] validate) => {
        parse_command_args::<$args>($name, $arguments).and_then(<$args>::validate)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] queued) => {
        parse_command_args::<$args>($name, $arguments).map(|args| with_queue_command(args, $queue))
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] queued_validate) => {
        parse_command_args::<$args>($name, $arguments)
            .and_then(<$args>::validate)
            .map(|args| with_queue_command(args, $queue))
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] custom $parse:path) => {
        $parse($arguments)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] named $parse:path) => {
        $parse($name, $arguments)
    };
    ([$name:expr, $arguments:expr, $queue:expr, $help:ty, $args:ty] interspersed) => {
        parse_command_args_with_policy::<$args>(
            $name,
            $arguments,
            PositionalOptionPolicy::InterspersedBeforeSeparator,
        )
        .and_then(<$args>::validate)
    };
}

macro_rules! typed_schema {
    (
        commands { $(($name:literal, $alias:expr, $kind:ident, $order:literal,
            $usage:literal, $variant:ident($($args:ty)?), $help:ty, [$($policy:tt)*]);)* }
        delegated { $(($raw:literal, $raw_alias:expr, $raw_kind:ident, $raw_order:literal,
            $raw_usage:literal, $delegate:ident);)* }
        local { $(($local:literal, $local_args:ty, $local_usage:literal);)* }
        aliases { $(($expansion_alias:literal, $expansion:literal, $expansion_args:ty);)* }
    ) => {
        #[derive(Debug, Clone)]
        pub(crate) enum Command {
            Noop,
            ApplyParseTimeAssignments(String),
            DeferredDimensionError(std::sync::Arc<clap::Error>),
            $($variant $(($args))?,)*
            Unsupported(UnsupportedCommandArgs),
        }

        $(pub(crate) struct $variant;
        impl CommandSpec for $variant {
            type Arguments = argument_type!($($args)?);
            const NAME: &'static str = $name;
            fn help() -> clap::Command { typed_command::<$help>($name) }
            fn parse(arguments: Vec<String>, _queue: String) -> Result<Self::Arguments, clap::Error> {
                parse_policy!([$name, arguments, _queue, $help, $($args)?] $($policy)*)
            }
        })*

        pub(crate) const DOCUMENTED_CLI_ALIASES: &[DocumentedCliAlias] = &[
            $(DocumentedCliAlias { alias: $expansion_alias, expansion: $expansion },)*
        ];

        pub(crate) fn command_help(name: &str) -> Option<clap::Command> {
            Some(match name {
                $(<$variant as CommandSpec>::NAME => <$variant as CommandSpec>::help(),)*
                $($raw => delegates::$delegate($raw).help,)*
                $($local => typed_command::<$local_args>($local),)*
                $($expansion_alias => typed_command::<$expansion_args>($expansion_alias)
                    .about(concat!("Alias for ", $expansion)),)*
                _ => return None,
            })
        }

        pub(crate) fn local_commands() -> impl Iterator<Item = clap::Command> {
            [$(typed_command::<$local_args>($local),)*].into_iter()
        }

        pub(crate) fn parse_command(
            name: String, arguments: Vec<String>, queue: String,
        ) -> Result<Command, clap::Error> {
            match name.as_str() {
                $(<$variant as CommandSpec>::NAME => <$variant as CommandSpec>::parse(arguments, queue)
                    .map(|args| wrap_command!($variant($($args)?), args)),)*
                $($raw => delegates::parse(name, arguments, delegates::$delegate($raw)),)*
                _ => Ok(Command::Unsupported(UnsupportedCommandArgs { name, arguments })),
            }
        }
    };
}
rmux_core::command_schema!(typed_schema);

const JSON_OUTPUT_HELP: &str = "Emit JSON results on stdout; command errors use text on stderr with a nonzero exit and empty stdout. Supported unsuccessful results remain JSON";

#[cfg(test)]
mod json_help_tests {
    #[test]
    fn every_json_argument_documents_success_and_error_channels() {
        let commands = super::super::completion_command();
        let mut checked = 0;
        for command in commands.get_subcommands() {
            if let Some(argument) = command
                .get_arguments()
                .find(|arg| arg.get_long() == Some("json"))
            {
                let help = argument.get_help().expect("JSON flag has help").to_string();
                for contract in ["success", "stdout", "errors", "stderr", "nonzero"] {
                    assert!(help.contains(contract), "{}: {help}", command.get_name());
                }
                checked += 1;
            }
        }
        assert!(checked >= 14, "JSON command coverage: {checked}");
    }
}

pub(crate) fn typed_command<T: Args>(name: &'static str) -> clap::Command {
    T::augment_args(empty_command(name))
        .args_override_self(true)
        .mut_args(|argument| {
            let argument = if argument.get_long() == Some("json") {
                argument.help(JSON_OUTPUT_HELP)
            } else {
                argument
            };
            if argument_requires_value(&argument)
                && (argument.get_short().is_some() || argument.get_long().is_some())
            {
                // Required values consume the next token, including an option
                // or `--`, in the parser, help, completion and tiny projections.
                argument.allow_hyphen_values(true)
            } else {
                argument
            }
        })
}

pub(crate) fn empty_command(name: &'static str) -> clap::Command {
    clap::Command::new(name)
        .no_binary_name(true)
        .disable_help_flag(true)
        .disable_help_subcommand(true)
        .arg(
            clap::Arg::new("help")
                .long("help")
                .action(ArgAction::Help)
                .help("Print help"),
        )
}

mod delegates;

#[derive(Debug, Clone, Args)]
pub(crate) struct DiagnoseArgs {
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "json")]
    human: bool,
    #[arg(long, action = ArgAction::SetTrue)]
    pub(crate) json: bool,
}

pub(crate) enum DiagnoseArgumentError {
    Help,
    Invalid(String),
}

pub(crate) fn parse_diagnose_args(
    arguments: &[OsString],
) -> Result<DiagnoseArgs, DiagnoseArgumentError> {
    let command = schema::command_help("diagnose").expect("declared CLI-only command");
    let stop = arguments.iter().enumerate().find_map(|(index, argument)| {
        let error = match argument.to_str() {
            None => DiagnoseArgumentError::Invalid(
                "rmux diagnose: arguments must be valid UTF-8".to_owned(),
            ),
            Some("--help") => DiagnoseArgumentError::Help,
            Some(argument) => {
                if command.get_arguments().any(|arg| {
                    arg.get_long()
                        .is_some_and(|long| argument.strip_prefix("--") == Some(long))
                }) {
                    return None;
                }
                DiagnoseArgumentError::Invalid(format!(
                    "rmux diagnose: unknown argument '{argument}'"
                ))
            }
        };
        Some((index, error))
    });
    // Resolve earlier conflicts before reporting a later help/invalid token,
    // preserving the diagnostic command's left-to-right error precedence.
    let end = stop.as_ref().map_or(arguments.len(), |(index, _)| *index);
    let matches = command
        .try_get_matches_from(&arguments[..end])
        .map_err(|error| {
            DiagnoseArgumentError::Invalid(match error.kind() {
                clap::error::ErrorKind::ArgumentConflict => {
                    "rmux diagnose: choose only one of --human or --json".to_owned()
                }
                _ => error.to_string(),
            })
        })?;
    if let Some((_, error)) = stop {
        return Err(error);
    }
    DiagnoseArgs::from_arg_matches(&matches)
        .map_err(|error| DiagnoseArgumentError::Invalid(error.to_string()))
}
