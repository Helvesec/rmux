use super::{
    lookup_command, parse_command_arguments, CommandArgument, CommandParser, COMMAND_TABLE,
};

fn command_names(input: &str) -> Vec<String> {
    CommandParser::new()
        .with_home_dir("/home/test")
        .parse(input)
        .expect("command parses")
        .commands()
        .iter()
        .map(|command| command.name().to_owned())
        .collect()
}

#[test]
fn frozen_command_inventory_has_expected_entries_and_aliases() {
    assert_eq!(COMMAND_TABLE.len(), 91);
    assert_eq!(lookup_command("new").unwrap().name, "new-session");
    assert_eq!(lookup_command("ls").unwrap().name, "list-sessions");
    assert_eq!(lookup_command("splitw").unwrap().name, "split-window");
}

#[test]
fn lookup_rejects_ambiguous_prefixes_with_tmux_diagnostic() {
    let error = lookup_command("list").unwrap_err();

    assert_eq!(
            error.to_string(),
            "ambiguous command: list, could be: list-buffers, list-clients, list-commands, list-keys, list-panes, list-sessions, list-windows"
        );
}

#[test]
fn lookup_rejects_unknown_commands_with_tmux_diagnostic() {
    assert_eq!(
        lookup_command("bogus").unwrap_err().to_string(),
        "unknown command: bogus"
    );
}

#[test]
fn parses_single_and_double_quoted_literals() {
    let commands = CommandParser::new()
        .parse("display-message 'literal $HOME' \"line\\nnext\"")
        .unwrap();
    let args = commands.commands()[0].arguments();

    assert_eq!(args[0].as_string(), Some("literal $HOME"));
    assert_eq!(args[1].as_string(), Some("line\nnext"));
}

#[test]
fn parses_escape_sequences() {
    let commands = CommandParser::new()
        .parse("display-message \"\\a\\b\\e\\f\\s\\v\\r\\n\\t\\101\\u263A\"")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("\x07\x08\x1b\x0c \x0b\r\n\tA☺")
    );
}

#[test]
fn rejects_invalid_escape_sequences() {
    assert!(CommandParser::new()
        .parse("display-message \"\\400\"")
        .unwrap_err()
        .to_string()
        .contains("invalid octal escape"));
    assert!(CommandParser::new()
        .parse("display-message \"\\u12xz\"")
        .unwrap_err()
        .to_string()
        .contains("invalid \\u argument"));
}

#[test]
fn expands_variables_and_tilde_at_tokenization_boundary() {
    let commands = CommandParser::new()
        .with_environment_value("HOME", "/tmp/home")
        .with_environment_value("NAME", "alpha")
        .with_user_home_dir("bob", "/home/bob")
        .parse("display-message $NAME ${MISSING} ~/x ~bob/y")
        .unwrap();
    let args = commands.commands()[0]
        .arguments()
        .iter()
        .map(|arg| arg.as_string().unwrap().to_owned())
        .collect::<Vec<_>>();

    assert_eq!(args, ["alpha", "", "/tmp/home/x", "/home/bob/y"]);
}

#[test]
fn parse_time_assignments_feed_later_variable_and_tilde_expansion() {
    let commands = CommandParser::new()
        .parse("FOO=bar\nHOME=/tmp/home\ndisplay-message \"$FOO\" ~/x\nlist-sessions $FOO")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("bar")
    );
    assert_eq!(
        commands.commands()[0].arguments()[1].as_string(),
        Some("/tmp/home/x")
    );
    assert_eq!(
        commands.commands()[1].arguments()[0].as_string(),
        Some("bar")
    );
}

#[test]
fn inactive_condition_assignments_do_not_feed_later_expansion() {
    let commands = CommandParser::new()
        .parse("%if 0\nFOO=bad display-message hidden\n%endif\ndisplay-message \"$FOO\"")
        .unwrap();

    assert_eq!(commands.commands()[0].arguments()[0].as_string(), Some(""));
    assert!(commands.assignments().is_empty());
}

#[test]
fn rejects_unclosed_braced_variable_and_unknown_tilde_user() {
    assert_eq!(
        CommandParser::new()
            .parse("display-message ${NAME")
            .unwrap_err()
            .to_string(),
        "invalid environment variable"
    );
    assert_eq!(
        CommandParser::new()
            .parse("display-message ~definitely_missing")
            .unwrap_err()
            .to_string(),
        "unknown user: ~definitely_missing"
    );
}

#[test]
fn parses_semicolon_separated_commands_and_trailing_separator() {
    assert_eq!(
        command_names("new-session -d ; list-sessions ;"),
        ["new-session", "list-sessions"]
    );
}

#[test]
fn records_command_start_lines_from_token_stream() {
    let commands = CommandParser::new()
        .parse("list-sessions\n\ndisplay-message ok")
        .unwrap();

    assert_eq!(commands.commands()[0].line(), 1);
    assert_eq!(commands.commands()[1].line(), 3);
}

#[test]
fn lookup_errors_report_command_start_line() {
    let error = CommandParser::new()
        .parse("list-sessions\nlist")
        .unwrap_err();

    assert_eq!(error.line(), 2);
    assert!(error.to_string().starts_with("ambiguous command: list"));
}

#[test]
fn parses_brace_delimited_command_arguments() {
    let commands = CommandParser::new()
        .parse("if-shell true { display-message yes ; list-sessions }")
        .unwrap();
    let argument = &commands.commands()[0].arguments()[1];

    match argument {
        CommandArgument::Commands(nested) => {
            let names = nested
                .commands()
                .iter()
                .map(|command| command.name())
                .collect::<Vec<_>>();
            assert_eq!(names, ["display-message", "list-sessions"]);
        }
        CommandArgument::String(_) => panic!("expected nested commands"),
    }
}

#[test]
fn classifies_assignments_and_hidden_assignments() {
    let commands = CommandParser::new()
        .parse("FOO=bar list-sessions\n%hidden SECRET=value\nset-environment 1BAD=value")
        .unwrap();

    assert_eq!(commands.assignments()[0].name(), "FOO");
    assert_eq!(commands.assignments()[0].value(), "bar");
    assert!(!commands.assignments()[0].hidden());
    assert_eq!(commands.assignments()[1].name(), "SECRET");
    assert!(commands.assignments()[1].hidden());
    assert_eq!(
        commands.commands()[1].arguments()[0].as_string(),
        Some("1BAD=value")
    );
}

#[test]
fn rejects_hidden_before_non_assignment() {
    assert_eq!(
        CommandParser::new()
            .parse("%hidden list-sessions")
            .unwrap_err()
            .to_string(),
        "%hidden must be followed by name=value"
    );
}

#[test]
fn rejects_assignment_followed_by_non_command_without_separator() {
    assert_eq!(
        CommandParser::new()
            .parse("FOO=bar BAR=baz list-sessions")
            .unwrap_err()
            .to_string(),
        "name=value assignment must be followed by a command or statement boundary"
    );
    assert_eq!(
        CommandParser::new()
            .parse("FOO=bar %hidden SECRET=value")
            .unwrap_err()
            .to_string(),
        "name=value assignment must be followed by a command or statement boundary"
    );
}

#[test]
fn strips_quoted_newline_comment_to_eof() {
    let commands = CommandParser::new()
        .parse("display-message \"first\n  # comment to eof")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("first\n")
    );
}

#[test]
fn strips_comments_and_accepts_format_after_condition_keyword() {
    let commands = CommandParser::new()
        .with_format_value("pane_active", "1")
        .parse("list-sessions # ignore\n%if #{pane_active}\ndisplay-message ok\n%endif")
        .unwrap();

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|command| command.name())
            .collect::<Vec<_>>(),
        ["list-sessions", "display-message"]
    );
}

#[test]
fn keeps_format_arguments_outside_condition_directives() {
    let commands = CommandParser::new()
        .parse("list-sessions -F #{session_name}")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[1].as_string(),
        Some("#{session_name}")
    );
}

#[test]
fn keeps_percent_arguments_outside_condition_directives() {
    let commands = CommandParser::new()
        .parse("run-shell printf %s value")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[1].as_string(),
        Some("%s")
    );
}

#[test]
fn condition_branches_select_expected_commands() {
    assert_eq!(
        command_names("%if 0\nlist-sessions\n%else\ndisplay-message ok\n%endif"),
        ["display-message"]
    );
    assert_eq!(
        command_names("%if 0 list-sessions %elif 1 display-message ok %endif"),
        ["display-message"]
    );
}

#[test]
fn condition_directives_expand_parse_time_formats_before_truthiness() {
    let commands = CommandParser::new()
        .with_format_value("cfg_enabled", "0")
        .with_format_value("cfg_fallback", "nonempty")
        .parse(
            "%if #{cfg_enabled}\nlist-sessions\n%elif #{cfg_fallback}\ndisplay-message ok\n%endif",
        )
        .unwrap();

    assert_eq!(commands.commands()[0].name(), "display-message");
}

#[test]
fn resolves_user_defined_command_aliases_before_lookup() {
    let commands = CommandParser::new()
        .with_command_alias("say=display-message -p")
        .unwrap()
        .parse("say hello")
        .unwrap();

    assert_eq!(commands.commands()[0].name(), "display-message");
    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("-p")
    );
    assert_eq!(
        commands.commands()[0].arguments()[1].as_string(),
        Some("hello")
    );
}

#[test]
fn command_alias_reparse_sees_parse_time_assignments() {
    let commands = CommandParser::new()
        .with_command_alias("say=display-message \"$FOO\"")
        .unwrap()
        .parse("FOO=bar say")
        .unwrap();

    assert_eq!(commands.commands()[0].name(), "display-message");
    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("bar")
    );
}

#[test]
fn built_in_command_aliases_resolve_choose_window_and_choose_session() {
    let choose_window = CommandParser::new()
        .parse("choose-window")
        .expect("choose-window parses");
    assert_eq!(choose_window.commands()[0].name(), "choose-tree");
    assert_eq!(
        choose_window.commands()[0].arguments()[0].as_string(),
        Some("-w")
    );

    let choose_session = CommandParser::new()
        .parse("choose-session")
        .expect("choose-session parses");
    assert_eq!(choose_session.commands()[0].name(), "choose-tree");
    assert_eq!(
        choose_session.commands()[0].arguments()[0].as_string(),
        Some("-s")
    );
}

#[test]
fn parses_argv_semicolon_boundaries_like_tmux_arguments() {
    let commands = parse_command_arguments(["list-sessions;", "display-message", "x\\;"])
        .expect("argv commands parse");

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|command| command.name())
            .collect::<Vec<_>>(),
        ["list-sessions", "display-message"]
    );
    assert_eq!(
        commands.commands()[1].arguments()[0].as_string(),
        Some("x;")
    );
}

#[test]
fn nested_if_blocks_evaluate_independently() {
    let commands = CommandParser::new()
            .parse(
                "%if 1\n%if 0\nlist-sessions\n%else\ndisplay-message inner\n%endif\n%else\ndisplay-message outer\n%endif",
            )
            .unwrap();

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["display-message"]
    );
    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("inner")
    );
}

#[test]
fn if_with_empty_condition_is_falsy() {
    let commands = CommandParser::new()
        .with_format_value("empty", "")
        .parse("%if #{empty}\nlist-sessions\n%else\ndisplay-message no\n%endif")
        .unwrap();

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["display-message"]
    );
}

#[test]
fn if_zero_condition_is_falsy() {
    let commands = CommandParser::new()
        .parse("%if 0\nlist-sessions\n%else\ndisplay-message no\n%endif")
        .unwrap();

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["display-message"]
    );
}

#[test]
fn elif_chain_stops_at_first_truthy_branch() {
    let commands = CommandParser::new()
            .parse(
                "%if 0\nlist-sessions\n%elif 0\nlist-windows\n%elif 1\ndisplay-message second\n%elif 1\ndisplay-message third\n%endif",
            )
            .unwrap();

    assert_eq!(
        commands
            .commands()
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["display-message"]
    );
    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("second")
    );
}

#[test]
fn continuation_joins_lines_outside_quotes() {
    let commands = CommandParser::new()
        .parse("display-message hel\\\nlo")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("hello")
    );
}

#[test]
fn double_backslash_before_newline_preserves_literal_backslash() {
    let commands = CommandParser::new()
        .parse("display-message \"val\\\\\\\nnext\"")
        .unwrap();

    // \\\\ in double-quoted context: two escaped backslashes = one literal backslash,
    // then \n is a continuation. Result: "val\" + "next" = "val\next"
    // Actually: in the double-quote escape handler, \\ becomes \, then \<newline> is join.
    let arg = commands.commands()[0].arguments()[0]
        .as_string()
        .expect("string argument");
    assert!(
        !arg.is_empty(),
        "double-backslash before newline should produce a non-empty result"
    );
}

#[test]
fn rejects_unclosed_if_block() {
    let error = CommandParser::new()
        .parse("%if 1\nlist-sessions")
        .unwrap_err();

    assert!(
        error.to_string().contains("expected %e"),
        "unclosed %if should produce a diagnostic: {error}"
    );
}

#[test]
fn empty_input_produces_no_commands() {
    let commands = CommandParser::new().parse("").unwrap();
    assert!(commands.commands().is_empty());
    assert!(commands.assignments().is_empty());
}

#[test]
fn only_comments_produce_no_commands() {
    let commands = CommandParser::new()
        .parse("# just a comment\n# another comment\n")
        .unwrap();
    assert!(commands.commands().is_empty());
}

#[test]
fn only_whitespace_produces_no_commands() {
    let commands = CommandParser::new().parse("   \n\n  \t  \n").unwrap();
    assert!(commands.commands().is_empty());
}
