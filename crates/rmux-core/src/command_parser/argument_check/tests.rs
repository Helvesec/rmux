use super::{check_tmux_arguments, TmuxArgumentTemplate, TMUX_ARGUMENT_TEMPLATES};
use crate::command_inventory::{command_short_option_spec, RMUX_EXTENSION_COMMANDS};
use crate::command_parser::{CommandParser, COMMAND_TABLE};

fn check(line: &str) -> Result<(), String> {
    let commands = CommandParser::new()
        .parse(line)
        .unwrap_or_else(|error| panic!("{line:?} must parse lexically: {error:?}"));
    let [command] = commands.commands() else {
        panic!("{line:?} must hold one command");
    };
    check_tmux_arguments(command).map_err(|error| error.to_string())
}

#[test]
fn tmux_measured_argument_errors_keep_their_cause() {
    // tmux 3.7b, private socket: each line inside a -C client prints
    // "parse error: <cause>" and `source-file -n` prints "<file>:1: <cause>".
    for (line, cause) in [
        ("display-message -p -:", "command display-message: invalid flag -:"),
        ("display-message -p '-:1: literal output'", "command display-message: invalid flag -:"),
        ("display -Q", "command display-message: unknown flag -Q"),
        ("display-message -pZ", "command display-message: unknown flag -Z"),
        ("display-message -t", "command display-message: -t expects an argument"),
        ("display-message -p a b", "command display-message: too many arguments (need at most 1)"),
        ("wait-for", "command wait-for: too few arguments (need at least 1)"),
        ("wait-for a b", "command wait-for: too many arguments (need at most 1)"),
        ("send-keys -t", "command send-keys: -t expects an argument"),
        ("new-window -y", "command new-window: unknown flag -y"),
        ("bind-key", "command bind-key: too few arguments (need at least 1)"),
        ("set-option -g", "command set-option: too few arguments (need at least 1)"),
        ("display-message -c { display -p x }", "command display-message: -c argument must be a string"),
        (
            "display-message '-?'",
            "usage: display-message [-aCIlNpv] [-c target-client] [-d delay] [-F format] [-t target-pane] [message]",
        ),
    ] {
        assert_eq!(check(line), Err(cause.to_owned()), "{line}");
    }
}

#[test]
fn tmux_accepted_argument_shapes_pass() {
    for line in [
        "display-message -p ok",
        "display-message -p -- -:",
        "display-message -p -",
        "display-message -tfoo -p x",
        "display-message -t -: -p x",
        "display-message -pt alpha x",
        "wait-for ''",
        "wait-for -S ''",
        "wait-for -L -U x",
        "send-keys -t alpha a b c d",
        "if-shell -F 1 { display -Q }",
        "kill-session -t missing",
        "set-option -g nosuch-opt 1",
    ] {
        assert_eq!(check(line), Ok(()), "{line}");
    }
}

#[test]
fn every_tmux_command_has_one_template_and_extensions_have_none() {
    for entry in COMMAND_TABLE {
        assert_eq!(
            TMUX_ARGUMENT_TEMPLATES
                .iter()
                .filter(|template| template.name == entry.name)
                .count(),
            1,
            "{}",
            entry.name
        );
    }
    assert_eq!(TMUX_ARGUMENT_TEMPLATES.len(), COMMAND_TABLE.len());
    for extension in RMUX_EXTENSION_COMMANDS {
        assert!(TmuxArgumentTemplate::for_command(extension.name).is_none());
    }
}

#[test]
fn rmux_only_short_options_are_the_known_extensions() {
    // RMUX implements these flags beyond tmux 3.7b; the check must accept them.
    let mut extra = Vec::new();
    for template in TMUX_ARGUMENT_TEMPLATES {
        let Some(spec) = command_short_option_spec(template.name) else {
            continue;
        };
        for flag in ('a'..='z').chain('A'..='Z').chain('0'..='9') {
            let tmux_knows =
                template.boolean_flags.contains(flag) || template.value_flags.contains(flag);
            if !tmux_knows && (spec.is_boolean(flag) || spec.takes_value(flag)) {
                extra.push(format!("{} -{flag}", template.name));
            }
        }
    }
    assert_eq!(extra, ["resize-pane -X", "resize-pane -Y"]);
    assert_eq!(check("resize-pane -X 3 -Y 4"), Ok(()));
    assert_eq!(
        check("resize-pane -X"),
        Err("command resize-pane: -X expects an argument".to_owned())
    );
}
