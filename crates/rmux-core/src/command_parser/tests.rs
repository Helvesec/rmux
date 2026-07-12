use super::{
    lookup_command, parse_command_arguments, CommandArgument, CommandParseErrorKind, CommandParser,
    COMMAND_TABLE, SOURCE_FILE_MAX_COMMAND_BYTES,
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
    assert_eq!(COMMAND_TABLE.len(), 90);
    assert_eq!(lookup_command("new").unwrap().name, "new-session");
    assert_eq!(lookup_command("ls").unwrap().name, "list-sessions");
    assert_eq!(lookup_command("splitw").unwrap().name, "split-window");
    assert_eq!(lookup_command("cap").unwrap().name, "capture-pane");
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
    assert_eq!(
        lookup_command("wait-pane").unwrap_err().to_string(),
        "unknown command: wait-pane"
    );
    assert_eq!(
        lookup_command("new-pane").unwrap_err().to_string(),
        "unknown command: new-pane"
    );
    assert_eq!(
        lookup_command("newp").unwrap_err().to_string(),
        "unknown command: newp"
    );
}

#[test]
fn tmux_parser_rejects_client_only_rmux_extensions() {
    let error = CommandParser::new()
        .parse("wait-pane --pane-exit")
        .expect_err("client-only extension must not parse in tmux/source-file parser");
    assert_eq!(error.to_string(), "unknown command: wait-pane");
}

#[test]
fn rejects_commands_longer_than_tmux_command_buffer() {
    let long_argument = "x".repeat(17 * 1024);
    let error = CommandParser::new()
        .parse(&format!("display-message {long_argument}"))
        .unwrap_err();
    assert_eq!(error.to_string(), "command too long");

    let error = parse_command_arguments(["display-message", long_argument.as_str()]).unwrap_err();
    assert_eq!(error.to_string(), "command too long");
}

#[test]
fn large_comments_do_not_count_as_command_bytes() {
    let comments = "#\n".repeat(16 * 1024);
    let commands = CommandParser::new()
        .parse(&format!("{comments}display-message ok"))
        .unwrap();

    assert_eq!(commands.commands().len(), 1);
    assert_eq!(commands.commands()[0].name(), "display-message");
}

#[test]
fn source_file_parser_accepts_long_commands_without_relaxing_argv_limit() {
    let long_argument = "x".repeat(20 * 1024);
    let commands = CommandParser::new()
        .with_max_command_bytes(SOURCE_FILE_MAX_COMMAND_BYTES)
        .parse(&format!("set-buffer -b big {long_argument}"))
        .unwrap();

    assert_eq!(commands.commands().len(), 1);
    assert_eq!(commands.commands()[0].name(), "set-buffer");
    assert!(parse_command_arguments(["set-buffer", "-b", "big", long_argument.as_str()]).is_err());
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

#[cfg(windows)]
#[test]
fn preserves_quoted_windows_drive_path_backslashes() {
    let commands = CommandParser::new()
        .parse(r#"set-environment -g AUDIT_PATH "C:\Users\RMUXUser\Documents\rmux""#)
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[2].as_string(),
        Some(r"C:\Users\RMUXUser\Documents\rmux")
    );
}

#[cfg(windows)]
#[test]
fn preserves_standard_escaped_backslashes_in_windows_drive_paths() {
    let commands = CommandParser::new()
        .parse(r#"set-environment -g AUDIT_PATH "C:\\Users\\RMUXUser""#)
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[2].as_string(),
        Some(r"C:\Users\RMUXUser")
    );
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
fn preserves_bare_marked_target_at_end_of_command() {
    let commands = CommandParser::new()
        .parse("select-pane -t ~")
        .expect("marked target parses");

    assert_eq!(commands.commands()[0].arguments()[1].as_string(), Some("~"));
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
fn serializes_embedded_command_lists_with_escaped_separators() {
    let commands = CommandParser::new()
        .parse(r#"run-shell "echo one" ; run-shell "echo two""#)
        .expect("command list parses");

    assert_eq!(
        commands.to_tmux_binding_string(),
        r#"run-shell "echo one" \; run-shell "echo two""#
    );
}

#[test]
fn binding_serialization_is_lossless_for_double_quotes_backslashes_and_controls() {
    for input in [
        r#"set-buffer -b got 'a"b\\c'"#,
        "set-buffer -b got 'quoted\"tab\there'",
        "set-buffer -b got 'quoted\"line\nthere'",
    ] {
        let commands = CommandParser::new().parse(input).expect("command parses");
        let serialized = commands.to_tmux_binding_string();
        let reparsed = CommandParser::new()
            .parse(&serialized)
            .expect("binding serialization reparses");

        assert_eq!(reparsed, commands, "serialized={serialized:?}");
    }
}

#[test]
fn serializes_arguments_with_tmux_quote_style() {
    for (input, expected) in [
        (
            r#"display-message "with space""#,
            r#"display-message "with space""#,
        ),
        (
            r#"display-message "has'dquote""#,
            r#"display-message "has'dquote""#,
        ),
        (
            r#"display-message 'has"quote'"#,
            r#"display-message 'has"quote'"#,
        ),
        (r#"display-message 'a b"c'"#, r#"display-message "a b\"c""#),
        ("display-message 'a\"\\b'", "display-message 'a\"\\\\b'"),
        (r#"display-message "~ foo""#, r#"display-message "\~ foo""#),
        (r#"display-message "%""#, r#"display-message \%"#),
        (
            r#"display-message "both'\"quotes""#,
            r#"display-message "both'\"quotes""#,
        ),
        (
            r#"display-message 'dollar$HOME'"#,
            r#"display-message "dollar\$HOME""#,
        ),
        (
            r#"display-message 'back\slash'"#,
            r#"display-message back\\slash"#,
        ),
        (
            r#"display-message 'hash#tag'"#,
            r#"display-message "hash#tag""#,
        ),
        (
            r#"display-message 'semi;colon'"#,
            r#"display-message "semi;colon""#,
        ),
        (
            r#"display-message 'brace{value}'"#,
            r#"display-message "brace{value}""#,
        ),
        (
            r#"display-message '~nosuchuser/file'"#,
            r#"display-message \~nosuchuser/file"#,
        ),
        (r#"display-message '~ foo'"#, r#"display-message "\~ foo""#),
        (r#"display-message '100%'"#, r#"display-message "100%""#),
        (
            "display-message \"has\\\"quote\tthere\"",
            "display-message 'has\"quote\\tthere'",
        ),
    ] {
        let commands = CommandParser::new().parse(input).expect("command parses");
        assert_eq!(commands.to_tmux_string(), expected);
    }
}

#[test]
fn serializes_dollar_arguments_for_lossless_reparse() {
    let commands = CommandParser::new()
        .parse(r#"if-shell -F '#{==:#{hook_session} #{hook_window},$0 @1}' 'display-message ok'"#)
        .expect("command parses");
    let serialized = commands.to_tmux_string();
    let reparsed = CommandParser::new()
        .parse(&serialized)
        .expect("serialized command reparses");

    assert_eq!(reparsed, commands);
}

#[test]
fn to_tmux_string_round_trip_preserves_dollar_anchor_in_substitution() {
    let commands = CommandParser::new()
        .parse(r#"display-message -p '#{s/$/Z/:session_name}'"#)
        .expect("command parses");
    let serialized = commands.to_tmux_string();

    assert!(
        serialized.contains("#{s/$/Z/:session_name}"),
        "dollar anchor should not be backslash-escaped in {serialized:?}"
    );

    let reparsed = CommandParser::new()
        .parse(&serialized)
        .expect("serialized command reparses");
    assert_eq!(reparsed, commands);
}

#[test]
fn to_tmux_string_escapes_control_whitespace_like_tmux_verbose_output() {
    let commands = CommandParser::new()
        .parse("display-message \"a b\\nc\" \"tab\\there\" \"car\\rret\"")
        .expect("command parses");

    assert_eq!(
        commands.to_tmux_string(),
        r#"display-message "a b\nc" tab\there car\rret"#
    );

    let reparsed = CommandParser::new()
        .parse(&commands.to_tmux_string())
        .expect("serialized command reparses");
    assert_eq!(reparsed, commands);
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
fn source_file_records_command_end_lines_for_multiline_tokens() {
    let commands = CommandParser::new()
        .parse_source_file("display-message \"a\nb\"\nset -g @after yes\n")
        .unwrap();

    assert_eq!(commands.commands()[0].start_line(), 1);
    assert_eq!(commands.commands()[0].line(), 2);
    assert_eq!(commands.commands()[1].start_line(), 3);
    assert_eq!(commands.commands()[1].line(), 3);
}

#[test]
fn source_file_comment_newline_counts_before_command_grouping() {
    let commands = CommandParser::new()
        .parse_source_file("display-message one # trailing comment\ndisplay-message two\n")
        .unwrap();

    assert_eq!(commands.commands()[0].start_line(), 1);
    assert_eq!(commands.commands()[0].line(), 2);
    assert_eq!(commands.commands()[1].start_line(), 2);
    assert_eq!(commands.commands()[1].line(), 2);
}

#[test]
fn source_file_lookup_errors_report_multiline_command_end_line() {
    let error = CommandParser::new()
        .parse_source_file("definitely-not-a-command \"a\nb\"\n")
        .unwrap_err();

    assert_eq!(error.line(), 2);
    assert_eq!(error.kind(), CommandParseErrorKind::Lookup);
    assert!(error
        .to_string()
        .starts_with("unknown command: definitely-not-a-command"));
}

#[test]
fn source_file_rejects_unquoted_unknown_percent_directives_as_syntax_errors() {
    let error = CommandParser::new()
        .parse_source_file("%word\n")
        .unwrap_err();

    assert_eq!(error.line(), 1);
    assert_eq!(error.kind(), CommandParseErrorKind::Structural);
    assert_eq!(error.to_string(), "syntax error");
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
fn alias_lookup_errors_report_invocation_line() {
    let error = CommandParser::new()
        .with_command_alias("badalias=definitely-not-a-command")
        .expect("valid alias")
        .parse("list-sessions\nbadalias\nlist-windows")
        .unwrap_err();

    assert_eq!(error.line(), 2);
    assert_eq!(error.kind(), CommandParseErrorKind::Lookup);
    assert!(error
        .to_string()
        .starts_with("unknown command: definitely-not-a-command"));
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
fn hoists_nested_parse_time_assignments_in_lexical_order_and_reparses_losslessly() {
    let commands = CommandParser::new()
        .parse(
            "OUTER=first ; if-shell -F 0 { FALSE_ONLY=no ; %hidden SECRET='two words' } { TRUE_ONLY=yes } ; set-hook -g after-new-window { HOOK=hooked ; if-shell -F 1 { DEEP=deep } } ; OUTER=last",
        )
        .expect("nested assignments parse");

    assert_eq!(
        commands
            .assignments()
            .iter()
            .map(|assignment| (assignment.name(), assignment.value(), assignment.hidden()))
            .collect::<Vec<_>>(),
        [
            ("OUTER", "first", false),
            ("FALSE_ONLY", "no", false),
            ("SECRET", "two words", true),
            ("TRUE_ONLY", "yes", false),
            ("HOOK", "hooked", false),
            ("DEEP", "deep", false),
            ("OUTER", "last", false),
        ]
    );
    for command in commands.commands() {
        for argument in command.arguments() {
            if let CommandArgument::Commands(nested) = argument {
                assert!(
                    nested.assignments().is_empty(),
                    "nested assignments must be owned by the parse root"
                );
            }
        }
    }

    let rendered = commands.to_tmux_reparse_string();
    let reparsed = CommandParser::new()
        .parse(&rendered)
        .expect("rendered assignments reparse");
    assert_eq!(reparsed, commands, "lossless reparse string: {rendered}");
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
fn source_file_comments_unquoted_hash_formats_like_tmux() {
    let commands = CommandParser::new()
        .with_format_value("version", "3.4")
        .parse_source_file(
            "%if #{>=:#{version},3.2}\n\
             set-option -g extended-keys #{?#{||:0,0},on,off}\n\
             %endif\n",
        )
        .unwrap();

    assert_eq!(commands.commands().len(), 1);
    assert_eq!(commands.commands()[0].name(), "set-option");
    let args = commands.commands()[0].arguments();
    assert_eq!(args.len(), 2);
    assert_eq!(args[0].as_string(), Some("-g"));
    assert_eq!(args[1].as_string(), Some("extended-keys"));
}

#[test]
fn source_file_preserves_quoted_hash_formats_and_condition_formats() {
    let commands = CommandParser::new()
        .with_format_value("pane_active", "1")
        .parse_source_file(
            "%if #{pane_active}\n\
             list-sessions -F '#{session_name}'\n\
             %endif\n",
        )
        .unwrap();

    assert_eq!(commands.commands().len(), 1);
    assert_eq!(commands.commands()[0].name(), "list-sessions");
    assert_eq!(
        commands.commands()[0].arguments()[1].as_string(),
        Some("#{session_name}")
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
        .parse_arguments(["run-shell", "printf %s value"])
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("printf %s value")
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
fn resolved_command_alias_option_entries_replace_default_aliases() {
    let commands = CommandParser::new()
        .with_command_aliases(["say=display-message -p"])
        .parse("say hello")
        .expect("custom alias should parse");

    assert_eq!(commands.commands()[0].name(), "display-message");
    assert!(
        CommandParser::new()
            .with_command_aliases(["say=display-message -p"])
            .parse("choose-window")
            .is_err(),
        "runtime command-alias array should be the source of truth"
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
fn continuation_joins_crlf_lines() {
    let commands = CommandParser::new()
        .parse("display-message hel\\\r\nlo")
        .unwrap();

    assert_eq!(
        commands.commands()[0].arguments()[0].as_string(),
        Some("hello")
    );
}

#[test]
fn carriage_return_line_endings_are_normalized() {
    let commands = CommandParser::new()
        .parse("display-message first\rdisplay-message second")
        .unwrap();

    assert_eq!(commands.commands().len(), 2);
    assert_eq!(
        commands.commands()[1].arguments()[0].as_string(),
        Some("second")
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

#[test]
fn deeply_nested_command_blocks_fail_closed_instead_of_overflowing() {
    // `{ … }` blocks recurse through parse_command -> parse_until. Without a cap
    // this overflows the native stack (SIGABRT). 400 > the depth cap, and the cap
    // returns a parse error before the recursion ever reaches the stack limit.
    let depth = 400;
    let input = "if-shell true { ".repeat(depth) + &"}".repeat(depth);
    let error = CommandParser::new().parse(&input).unwrap_err();
    assert!(
        error.to_string().contains("command nesting too deep"),
        "expected nesting error, got: {error}"
    );
}

#[test]
fn deeply_nested_conditionals_fail_closed_instead_of_overflowing() {
    // The %if branch recurses through parse_condition -> parse_until, a second
    // unbounded cycle that the brace cap must also cover (shared at parse_until).
    let depth = 400;
    let input = "%if 1\n".repeat(depth) + &"%endif\n".repeat(depth);
    let error = CommandParser::new().parse(&input).unwrap_err();
    assert!(
        error.to_string().contains("command nesting too deep"),
        "expected nesting error, got: {error}"
    );
}

#[test]
fn modest_block_and_conditional_nesting_still_parses() {
    // The cap must not regress real configs: moderate nesting still parses.
    let depth = 32;
    let braces = "if-shell true { ".repeat(depth) + "display-message ok " + &"}".repeat(depth);
    CommandParser::new()
        .parse(&braces)
        .expect("nested blocks parse");
    let conds = "%if 1\n".repeat(depth) + "display-message ok\n" + &"%endif\n".repeat(depth);
    CommandParser::new()
        .parse(&conds)
        .expect("nested conditionals parse");
}
