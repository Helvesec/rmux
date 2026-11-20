//! Read the existing runtime registries rather than declare another command table.

use super::Error;
use crate::option_registry::{self, DefaultValue, GlobalRoot, OptionValueType};
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct Snapshot {
    pub version: &'static str,
    pub defaults_platform: &'static str,
    pub top_level_help: String,
    pub commands: Vec<Command>,
    pub cli_aliases: Vec<Alias>,
    pub local_commands: Vec<Command>,
    pub options: Vec<ConfigOption>,
}

#[derive(Serialize)]
pub(super) struct Command {
    pub name: String,
    pub alias: Option<String>,
    pub help: String,
}

#[derive(Serialize)]
pub(super) struct Alias {
    pub name: String,
    pub expansion: String,
}

#[derive(Serialize)]
pub(super) struct ConfigOption {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub value_type: String,
    pub scopes: Vec<&'static str>,
    pub global_root: &'static str,
    pub defaults: Vec<&'static str>,
    pub default_platforms: Vec<&'static str>,
    pub array: bool,
    pub separator: &'static str,
}

pub(super) fn options() -> Vec<ConfigOption> {
    option_registry::registry()
        .iter()
        .map(|option| ConfigOption {
            name: option.name(),
            aliases: option.aliases(),
            value_type: match option.value_type() {
                OptionValueType::String => "string".into(),
                OptionValueType::Number { minimum, maximum } => {
                    format!("number {minimum}..={maximum}")
                }
                OptionValueType::Key => "key".into(),
                OptionValueType::Colour => "colour".into(),
                OptionValueType::Flag => "flag (off/on)".into(),
                OptionValueType::Choice(values) => format!("choice: {}", values.join(", ")),
                OptionValueType::Command => "command".into(),
            },
            scopes: [
                (option_registry::SCOPE_SERVER, "server"),
                (option_registry::SCOPE_SESSION, "session"),
                (option_registry::SCOPE_WINDOW, "window"),
                (option_registry::SCOPE_PANE, "pane"),
            ]
            .into_iter()
            .filter_map(|(mask, name)| (option.scope_mask() & mask != 0).then_some(name))
            .collect(),
            global_root: match option.global_root() {
                GlobalRoot::Server => "server",
                GlobalRoot::Session => "session",
                GlobalRoot::Window => "window",
            },
            defaults: defaults(option).0,
            default_platforms: defaults(option).1,
            array: option.is_array(),
            separator: option.separator(),
        })
        .collect()
}

fn defaults(option: &option_registry::OptionMetadata) -> (Vec<&'static str>, Vec<&'static str>) {
    let unix = option.default_value_for_windows(false);
    let windows = option.default_value_for_windows(true);
    if unix != windows {
        let (DefaultValue::Scalar(unix), DefaultValue::Scalar(windows)) = (unix, windows) else {
            unreachable!("platform defaults are scalar registry declarations");
        };
        (vec![unix, windows], vec!["unix", "windows"])
    } else {
        match unix {
            DefaultValue::Scalar(value) => (vec![value], vec!["all"]),
            DefaultValue::Array(values) => (values.to_vec(), vec!["all"; values.len()]),
        }
    }
}

#[cfg(not(test))]
pub(super) fn implemented() -> Result<Snapshot, Error> {
    let mut cli = crate::cli_args::completion_command();
    let mismatches: Vec<_> = cli
        .get_subcommands()
        .filter(|command| {
            rmux_core::command_inventory::command_short_option_spec(command.get_name()).is_some()
        })
        .filter_map(|command| {
            crate::cli_args::schema::command_help(command.get_name())
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "command has no runtime schema: {}",
                        command.get_name()
                    ))
                })
                .and_then(|runtime| validate_flags(command, &runtime))
                .err()
                .map(|error| error.to_string())
        })
        .collect();
    if !mismatches.is_empty() {
        return Err(Error::Invalid(mismatches.join("; ")));
    }
    let mut commands = Vec::new();
    for entry in crate::cli_args::implemented_command_surface() {
        let command = cli.find_subcommand_mut(entry.name).ok_or_else(|| {
            Error::Invalid(format!(
                "implemented command has no CLI help: {}",
                entry.name
            ))
        })?;
        commands.push(Command {
            name: entry.name.into(),
            alias: entry.alias.map(str::to_owned),
            help: command.render_long_help().to_string(),
        });
    }
    let cli_aliases = crate::cli_args::documented_cli_aliases()
        .iter()
        .map(|alias| Alias {
            name: alias.alias.into(),
            expansion: alias.expansion.into(),
        })
        .collect();
    let mut local_commands = Vec::new();
    for mut command in crate::cli_args::schema::local_commands() {
        local_commands.push(Command {
            name: command.get_name().into(),
            alias: None,
            help: command.render_long_help().to_string(),
        });
    }
    // Subcommands are rendered separately. Top-level flags remain clap-owned.
    let top_level_help = cli.render_long_help().to_string();
    Ok(Snapshot {
        version: env!("CARGO_PKG_VERSION"),
        defaults_platform: "all (explicit unix/windows overrides)",
        top_level_help,
        commands,
        cli_aliases,
        local_commands,
        options: options(),
    })
}

pub(super) fn validate_flags(
    command: &clap::Command,
    runtime: &clap::Command,
) -> Result<(), Error> {
    use rmux_core::command_inventory::command_short_option_spec;
    let name = command.get_name();
    let spec = command_short_option_spec(name)
        .ok_or_else(|| Error::Invalid(format!("command has no runtime signature: {name}")))?;
    for arg in command.get_arguments() {
        if arg.is_hide_set() {
            continue;
        }
        if let Some(flag) = arg.get_long() {
            // Long CLI extensions need not appear in tmux's short-option usage.
            // Bind them to the separately obtained, typed runtime schema instead.
            let accepted = runtime.get_arguments().any(|signature| {
                signature.get_long() == Some(flag)
                    && signature.get_short() == arg.get_short()
                    && std::mem::discriminant(signature.get_action())
                        == std::mem::discriminant(arg.get_action())
                    && signature.get_num_args() == arg.get_num_args()
            });
            if !accepted {
                return Err(Error::Invalid(format!(
                    "CLI flag has no matching runtime signature: {name} --{flag}"
                )));
            }
        }
        if let Some(flag) = arg.get_short() {
            let takes_value = arg.get_action().takes_values();
            let optional_value = arg
                .get_num_args()
                .is_some_and(|range| range.min_values() == 0);
            let required_signature =
                rmux_core::command_inventory::render_list_commands_line(None, name, None);
            let required_value = required_signature.contains(&format!("-{flag} "));
            let accepted = if takes_value {
                spec.takes_value(flag)
                    || required_value
                    || (spec.is_boolean(flag) && optional_value)
            } else {
                spec.is_boolean(flag)
            };
            if !accepted {
                return Err(Error::Invalid(format!(
                    "CLI flag has no matching runtime signature: {name} -{flag}"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn implemented() -> Result<Snapshot, Error> {
    Err(Error::Invalid(
        "CLI projection is exercised by the real xtask binary acceptance".into(),
    ))
}
