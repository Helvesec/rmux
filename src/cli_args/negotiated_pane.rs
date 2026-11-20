use clap::{Arg, ArgAction};
use rmux_core::command_inventory::command_short_option_spec;

const SHORT_OPTIONS: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

pub(super) fn command(name: &'static str) -> clap::Command {
    let command = clap::Command::new(name)
        .no_binary_name(true)
        .disable_help_flag(true)
        .arg(Arg::new("help").long("help").action(ArgAction::Help));
    assert!(matches!(name, "new-pane" | "set-pane-placement"));
    let options = command_short_option_spec(name).expect("declared negotiated pane command");
    let mut command = command;
    for (index, short) in SHORT_OPTIONS.char_indices() {
        let id = &SHORT_OPTIONS[index..index + 1];
        if options.is_boolean(short) {
            command = command.arg(Arg::new(id).short(short).action(ArgAction::SetTrue));
        } else if options.takes_value(short) {
            command = command.arg(
                Arg::new(id)
                    .short(short)
                    .allow_hyphen_values(true)
                    .action(ArgAction::Append),
            );
        }
    }
    if name == "new-pane" {
        command.arg(
            Arg::new("shell-command")
                .num_args(0..)
                .trailing_var_arg(true)
                .allow_hyphen_values(true),
        )
    } else {
        command.arg(Arg::new("placement").value_parser(["floating", "tiled"]))
    }
}
