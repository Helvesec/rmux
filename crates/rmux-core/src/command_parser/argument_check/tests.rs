use super::{
    check_tmux_arguments, print_tmux_command_list, without_trailing_option_terminator,
    TmuxArgumentTemplate, TMUX_ARGUMENT_TEMPLATES,
};
use crate::command_inventory::{command_short_option_spec, RMUX_EXTENSION_COMMANDS};
use crate::command_parser::{
    CommandArgument, CommandParser, ParsedCommand, ParsedCommands, COMMAND_TABLE,
};

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

fn without_terminator(line: &str) -> Vec<String> {
    let commands = CommandParser::new()
        .parse(line)
        .unwrap_or_else(|error| panic!("{line:?} must parse lexically: {error:?}"));
    let [command] = commands.commands() else {
        panic!("{line:?} must hold one command");
    };
    without_trailing_option_terminator(command.clone())
        .arguments()
        .iter()
        .map(|argument| argument.as_string().unwrap_or("{}").to_owned())
        .collect()
}

#[test]
fn only_a_final_option_terminator_is_removed() {
    // tmux 3.7b runs each removed form as the same command without `--`.
    for (line, arguments) in [
        ("list-sessions --", &[][..]),
        ("ls --", &[][..]),
        ("list-sessions -r --", &["-r"][..]),
        ("list-sessions -F x --", &["-F", "x"][..]),
        ("list-sessions -rF x --", &["-rF", "x"][..]),
        ("list-sessions -F -- --", &["-F", "--"][..]),
        ("kill-server --", &[][..]),
        // Kept: a flag value, a separator before a positional, a second `--`
        // and a rejected option; below, every command without a tmux template.
        ("list-sessions -F --", &["-F", "--"][..]),
        ("list-sessions -- x", &["--", "x"][..]),
        ("list-sessions -- --", &["--", "--"][..]),
        ("display-message -p -- --bad", &["-p", "--", "--bad"][..]),
        ("list-sessions -Z --", &["-Z", "--"][..]),
        ("list-sessions -F", &["-F"][..]),
    ] {
        assert_eq!(without_terminator(line), arguments, "{line}");
    }
    for extension in RMUX_EXTENSION_COMMANDS {
        let command = ParsedCommand {
            name: extension.name.to_owned(),
            arguments: vec![CommandArgument::String("--".to_owned())],
            start_line: 1,
            line: 1,
        };
        assert_eq!(
            without_trailing_option_terminator(command.clone()),
            command,
            "{}",
            extension.name
        );
    }
}

fn print(input: &str) -> String {
    let commands = CommandParser::new()
        .parse(input)
        .unwrap_or_else(|error| panic!("{input:?} must parse: {error:?}"));
    print_tmux_command_list(&commands)
}

#[test]
fn hook_commands_print_as_tmux_shows_them() {
    // tmux 3.7b (private socket): `set-hook -g after-new-window '<input>'`
    // then `show-hooks -g after-new-window` prints `after-new-window[0] <shown>`.
    // Windows uses a leading numeric escape for words containing escapes so
    // its lexer cannot reinterpret them as literal native paths on reparse.
    // Keep exact platform spellings here; test value preservation below too.
    for (input, shown) in [
        ("list-sessions -F x --", "list-sessions -F x"),
        ("list-sessions -F x --   ", "list-sessions -F x"),
        ("list-sessions -F x", "list-sessions -F x"),
        ("list-sessions --", "list-sessions"),
        ("list-sessions -F --", "list-sessions -F --"),
        ("list-sessions -F -- --", "list-sessions -F --"),
        ("list-sessions -F -x", "list-sessions -F -x"),
        ("list-sessions -F \"a b\"", "list-sessions -F \"a b\""),
        ("display-message -p -- hello", "display-message -p hello"),
        ("display-message -p -", "display-message -p -"),
        (
            "display-message -p \"hello world\"",
            "display-message -p \"hello world\"",
        ),
        ("display-message -p 'it is'", "display-message -p \"it is\""),
        (
            "display-message -p '#{session_name}'",
            "display-message -p \"#{session_name}\"",
        ),
        (
            "display-message -p '$HOME'",
            if cfg!(windows) {
                r#"display-message -p "\044HOME""#
            } else {
                r#"display-message -p "\$HOME""#
            },
        ),
        ("display-message -p ''", "display-message -p ''"),
        (
            "display-message -p 'say \"hi\"'",
            if cfg!(windows) {
                r#"display-message -p "\163ay \"hi\"""#
            } else {
                r#"display-message -p "say \"hi\"""#
            },
        ),
        ("new-window -n foo -d", "new-window -d -n foo"),
        ("new-window -dn foo", "new-window -d -n foo"),
        ("new-window -dd", "new-window -dd"),
        ("neww -d", "new-window -d"),
        ("kill-session -C -a", "kill-session -Ca"),
        ("capture-pane -p -J -a", "capture-pane -Jap"),
        ("kill-pane -a -t alpha", "kill-pane -a -t alpha"),
        (
            "list-windows -t alpha -t beta",
            "list-windows -t alpha -t beta",
        ),
        ("select-pane -t:.+", "select-pane -t :.+"),
        ("split-window -h -l 10 --", "split-window -h -l 10"),
        ("run-shell -b -- 'echo hi'", "run-shell -b \"echo hi\""),
        ("set-option -g @x \"a b\"", "set-option -g @x \"a b\""),
        (
            "bind-key -n M-x list-sessions --",
            "bind-key -n M-x list-sessions --",
        ),
        (
            "if-shell -F '#{x}' 'display-message a' 'display-message b'",
            "if-shell -F \"#{x}\" \"display-message a\" \"display-message b\"",
        ),
        (
            "display-message -p a ; display-message -p b",
            "display-message -p a ; display-message -p b",
        ),
        (
            "display-message -p a\ndisplay-message -p b",
            "display-message -p a ; display-message -p b",
        ),
        (
            "list-sessions -F x -- ; list-windows --",
            "list-sessions -F x ; list-windows",
        ),
        (
            "if-shell true { display-message -p a }",
            "if-shell true { display-message -p a }",
        ),
        (
            "if-shell true { display-message -p a ; display-message -p b }",
            "if-shell true { display-message -p a ; display-message -p b }",
        ),
        (
            "if-shell true {\ndisplay-message -p a\ndisplay-message -p b\n}",
            "if-shell true { display-message -p a ; display-message -p b }",
        ),
        (
            "if-shell true { list-sessions -F x -- }",
            "if-shell true { list-sessions -F x }",
        ),
    ] {
        assert_eq!(print(input), shown, "{input:?}");
    }
}

#[test]
fn shown_hook_escapes_preserve_argument_values() {
    fn assert_same_values(actual: &ParsedCommands, expected: &ParsedCommands) {
        // Grouping belongs to the parser entry mode, not to argument values.
        assert_eq!(actual.commands().len(), expected.commands().len());
        for (actual, expected) in actual.commands().iter().zip(expected.commands()) {
            assert_eq!(actual.name(), expected.name());
            assert_eq!(actual.arguments().len(), expected.arguments().len());
            for (actual, expected) in actual.arguments().iter().zip(expected.arguments()) {
                match (actual, expected) {
                    (CommandArgument::Commands(actual), CommandArgument::Commands(expected)) => {
                        assert_same_values(actual, expected);
                    }
                    _ => assert_eq!(actual, expected),
                }
            }
        }
    }

    let parser = CommandParser::new().with_environment_value("HOME", "must-not-expand");
    for value in [
        "$HOME",
        r#"say "hi""#,
        "/$HOME",
        "C:/\x0b\r\n\t\\tail/$HOME/'\"",
        r"C:\new\tab\vspace\033\u1234",
        r"\\host\share\new\vspace",
        "é/$HOME",
    ] {
        // Positionals, flag values and the fallback for rejected arguments
        // all pass through the hook printer's escaping boundary.
        for arguments in [
            ["display-message", "-p", value],
            ["new-window", "-n", value],
            ["display-message", "-Z", value],
        ] {
            let commands = parser.parse_arguments(arguments).unwrap();
            let mut nested = ParsedCommands::default();
            nested.push_command(ParsedCommand {
                name: "if-shell".into(),
                arguments: vec![
                    CommandArgument::String("true".into()),
                    CommandArgument::Commands(commands.clone()),
                ],
                start_line: 1,
                line: 1,
            });
            for original in [commands, nested] {
                let shown = print_tmux_command_list(&original);
                for reparsed in [
                    parser.parse(&shown),
                    parser.parse_source_file(&shown),
                    parser.parse_one_group(&shown),
                ] {
                    let reparsed = reparsed.unwrap_or_else(|error| panic!("{shown:?}: {error}"));
                    assert_same_values(&reparsed, &original);
                }
            }
        }
    }
}

#[test]
fn a_leading_dash_positional_keeps_its_terminator_product_divergence() {
    // C-D98: tmux 3.7b shows `display-message -x`,
    // `display-message -p --` and `send-keys -l -t alpha -x`, which read back
    // as a flag or as a bare terminator. The shown command keeps `--` so it
    // means what the hook runs.
    for (input, shown) in [
        ("display-message -- -x", "display-message -- -x"),
        ("display-message -p -- --", "display-message -p -- --"),
        ("send-keys -t alpha -l -- -x", "send-keys -l -t alpha -- -x"),
    ] {
        assert_eq!(print(input), shown, "{input:?}");
    }
}

#[test]
fn shown_commands_read_back_to_the_same_command() {
    for input in [
        "new-window -n foo -dd -c /tmp",
        "display-message -- -x",
        "display-message -p -- --",
        "select-pane -t:.+",
        "if-shell -F '#{x}' { display-message -p -- -a ; list-sessions -F x -- } 'kill-pane'",
    ] {
        let shown = print(input);
        assert_eq!(print(&shown), shown, "{input:?} shown as {shown:?}");
    }
}

#[test]
fn commands_tmux_would_reject_or_does_not_know_keep_their_words() {
    for (input, shown) in [
        ("display-message -p hello -a", "display-message -p hello -a"),
        ("list-sessions -Z --", "list-sessions -Z --"),
        (
            "if-shell -Z true { list-sessions -F x -- }",
            "if-shell -Z true { list-sessions -F x }",
        ),
    ] {
        assert_eq!(print(input), shown, "{input:?}");
    }
}
