use rmux_core::command_parser::CommandEntry;

use super::*;

pub(crate) fn completion_command() -> clap::Command {
    let mut command = clap::Command::new("rmux")
        .disable_help_subcommand(true)
        .version(env!("CARGO_PKG_VERSION"))
        .args(
            RawCli::command()
                .get_arguments()
                .filter(|argument| argument.get_id() != "command")
                .cloned(),
        );

    for entry in implemented_command_surface() {
        command = command.subcommand(completion_subcommand(entry));
    }
    for alias in documented_cli_aliases() {
        command = command.subcommand(
            super::schema::command_help(alias.alias)
                .expect("declared expansion alias")
                .about(format!("Alias for {}", alias.expansion)),
        );
    }

    command.subcommands(super::schema::local_commands())
}

fn completion_subcommand(entry: &'static CommandEntry) -> clap::Command {
    let mut command = super::schema::command_help(entry.name)
        .expect("registered command has a typed help projection");
    if let Some(alias) = entry.alias {
        command = command.visible_alias(alias);
    }
    command
}
