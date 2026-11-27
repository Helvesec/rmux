use super::*;
use std::sync::OnceLock;

static IMPLEMENTED_COMMAND_SURFACE: OnceLock<Vec<&'static CommandEntry>> = OnceLock::new();

static IMPLEMENTED_COMMAND_HELP: OnceLock<String> = OnceLock::new();

fn build_implemented_command_help() -> String {
    let mut help = String::from("Commands:\n");
    for entry in implemented_command_surface() {
        help.push_str("  ");
        help.push_str(entry.name);
        if let Some(alias) = entry.alias {
            help.push_str(" (");
            help.push_str(alias);
            help.push(')');
        }
        help.push('\n');
    }

    help.push_str("\nBuilt-in command aliases:\n");
    for alias in documented_cli_aliases() {
        help.push_str("  ");
        help.push_str(alias.alias);
        help.push_str(" => ");
        help.push_str(alias.expansion);
        help.push('\n');
    }

    help.trim_end().to_owned()
}

pub(crate) fn implemented_command_surface() -> &'static [&'static CommandEntry] {
    IMPLEMENTED_COMMAND_SURFACE
        .get_or_init(|| {
            COMMAND_TABLE
                .iter()
                .chain(RMUX_EXTENSION_COMMANDS.iter())
                .collect()
        })
        .as_slice()
}

pub(super) fn implemented_command_help() -> &'static str {
    IMPLEMENTED_COMMAND_HELP
        .get_or_init(build_implemented_command_help)
        .as_str()
}

pub(crate) fn documented_cli_aliases() -> &'static [DocumentedCliAlias] {
    DOCUMENTED_CLI_ALIASES
}

use clap::Args;

#[derive(Debug, Clone, Args)]
pub(crate) struct ListCommandsArgs {
    #[arg(short = 'F', allow_hyphen_values = true)]
    pub(crate) format: Option<String>,
    #[arg(allow_hyphen_values = true)]
    pub(crate) command: Option<String>,
}
