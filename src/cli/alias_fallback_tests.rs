use std::ffi::OsStr;

use super::*;

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsStr::new).map(OsString::from).collect()
}

#[test]
fn command_arguments_skip_top_level_socket_options() {
    assert_eq!(
        command_arguments(&args(&["rmux", "-L", "demo", "hi", "there"])),
        Some(vec!["hi".to_owned(), "there".to_owned()])
    );
    assert_eq!(
        command_arguments(&args(&["rmux", "-Sdemo.sock", "hi"])),
        Some(vec!["hi".to_owned()])
    );
}

#[test]
fn argv_groups_preserve_escaped_semicolon_literals() {
    assert_eq!(
        split_literal_command_groups(&args(&[
            "display-message",
            "literal\\;",
            ";",
            "list-sessions;",
        ])),
        Some(vec![
            vec!["display-message".to_owned(), "literal;".to_owned()],
            vec!["list-sessions".to_owned()],
        ])
    );
}

#[test]
fn only_single_non_control_list_commands_invocations_are_intrinsically_local() {
    let local = RuntimeCommandInvocation {
        arguments: vec!["list-commands".to_owned()],
        groups: vec![vec!["FOO=bar".to_owned(), "list-commands".to_owned()]],
        control_mode: 0,
    };
    assert!(invocation_is_intrinsically_local(&local));

    let alias = RuntimeCommandInvocation {
        arguments: vec!["lscm".to_owned()],
        groups: vec![vec!["lscm".to_owned()]],
        control_mode: 0,
    };
    assert!(invocation_is_intrinsically_local(&alias));

    let unique_prefix = RuntimeCommandInvocation {
        arguments: vec!["list-com".to_owned()],
        groups: vec![vec!["list-com".to_owned()]],
        control_mode: 0,
    };
    assert!(invocation_is_intrinsically_local(&unique_prefix));

    let control = RuntimeCommandInvocation {
        arguments: vec!["list-commands".to_owned()],
        groups: vec![vec!["list-commands".to_owned()]],
        control_mode: 1,
    };
    assert!(!invocation_is_intrinsically_local(&control));

    let queue = RuntimeCommandInvocation {
        arguments: vec![
            "list-commands".to_owned(),
            ";".to_owned(),
            "list-sessions".to_owned(),
        ],
        groups: vec![
            vec!["list-commands".to_owned()],
            vec!["list-sessions".to_owned()],
        ],
        control_mode: 0,
    };
    assert!(!invocation_is_intrinsically_local(&queue));
}

#[test]
fn tmux_quote_preserves_command_separators_and_quotes_values() {
    assert_eq!(tmux_quote_argument(""), "''");
    assert_eq!(tmux_quote_argument(";"), ";");
    assert_eq!(tmux_quote_argument("xyz;"), "xyz;");
    assert_eq!(tmux_quote_argument("hello world;"), "'hello world';");
    assert_eq!(tmux_quote_argument("xyz\\;"), "'xyz;'");
    assert_eq!(tmux_quote_argument("semi;colon"), "'semi;colon'");
    assert_eq!(tmux_quote_argument("it's"), "'it'\\''s'");
}

#[test]
fn legacy_fallback_accepts_canonical_and_unambiguous_kill_server_prefixes() {
    assert!(queue_starts_with_kill_server(&[vec![
        "kill-server".to_owned(),
    ]]));
    assert!(queue_starts_with_kill_server(&[vec![
        "kill-serv".to_owned(),
    ]]));
    assert!(queue_starts_with_kill_server(&[
        vec!["kill-server".to_owned()],
        vec!["new-session".to_owned()],
    ]));
    assert!(!queue_starts_with_kill_server(&[vec![
        "kill-server".to_owned(),
        "unexpected".to_owned(),
    ]]));
    assert!(!queue_starts_with_kill_server(&[
        vec!["display-message".to_owned()],
        vec!["kill-server".to_owned()],
    ]));
}

#[test]
fn cold_start_probe_parses_only_the_first_command_group() {
    let invocation = args(&[
        "rmux",
        "new-session",
        "-d",
        "-s",
        "alpha",
        ";",
        "runtime-alias",
    ]);
    assert!(matches!(
        first_cold_start_command(&invocation),
        Some(Command::NewSession(_))
    ));

    let invalid_first = args(&["rmux", "not-a-command", ";", "new-session", "-d"]);
    assert!(first_cold_start_command(&invalid_first).is_none());
}

#[test]
fn cold_start_probe_accepts_parse_time_assignment_before_first_command() {
    let invocation = args(&[
        "rmux",
        "FOO=x",
        "BAR=y",
        "new-session",
        "-d",
        "-s",
        "alpha",
        ";",
        "runtime-alias",
    ]);

    assert!(matches!(
        first_cold_start_command(&invocation),
        Some(Command::NewSession(_))
    ));
}

#[test]
fn cold_start_probe_preserves_escaped_semicolon_arguments() {
    let invocation = args(&[
        "rmux",
        "FOO=x",
        "new-session",
        "-d",
        "-s",
        "alpha",
        "shell\\;",
        ";",
        "runtime-alias",
    ]);

    let Some(Command::NewSession(command)) = first_cold_start_command(&invocation) else {
        panic!("first command should remain new-session");
    };
    assert_eq!(command.command, ["shell;"]);
}

#[test]
fn cold_start_probe_keeps_later_creator_arguments_and_first_creator_selection() {
    let invocation = args(&[
        "rmux",
        "set-option",
        "-g",
        "status",
        "off;",
        "FOO=x",
        ";",
        "BAR=y",
        "new-session",
        "-d",
        "-s",
        "alpha",
        "shell\\;",
        ";",
        "start-server",
        ";",
        "runtime-alias",
    ]);

    let Some(Command::NewSession(command)) = first_cold_start_command(&invocation) else {
        panic!("the first creator should remain new-session");
    };
    assert_eq!(command.command, ["shell;"]);
    assert_eq!(
        command.session_name.as_ref().map(|name| name.as_str()),
        Some("alpha")
    );
    assert!(command.detached);
}

#[test]
fn cold_start_probe_keeps_later_start_server_web_options() {
    let invocation = args(&[
        "rmux",
        "list-commands",
        ";",
        "start-server",
        "--web-port",
        "8042",
        "--frontend-url",
        "https://queued.example.invalid/share",
        ";",
        "runtime-alias",
    ]);

    let Some(Command::StartServer(command)) = first_cold_start_command(&invocation) else {
        panic!("the creator should remain start-server");
    };
    assert_eq!(command.web_port, Some(8042));
    assert_eq!(
        command.web_frontend.as_deref(),
        Some("https://queued.example.invalid/share")
    );
}

#[test]
fn cold_start_probe_requires_a_creator_before_any_invalid_group() {
    for tail in [
        vec!["runtime-alias"],
        vec!["list-sessions"],
        vec!["runtime-alias", ";", "new-session", "-d"],
        vec!["set-option", "-!", ";", "new-session", "-d"],
    ] {
        let mut invocation = args(&["rmux", "set-option", "-g", "status", "off", ";"]);
        invocation.extend(args(&tail));
        assert!(first_cold_start_command(&invocation).is_none());
    }
}

#[test]
fn legacy_fallback_detects_aliases_in_any_command_group() {
    let groups = vec![
        vec!["list-sessions".to_owned()],
        vec!["probe".to_owned(), "argument".to_owned()],
    ];
    let aliases = vec!["probe=display-message -p ok".to_owned()];

    assert!(queue_uses_server_alias(&groups, &aliases));
    assert!(!queue_uses_server_alias(
        &[vec!["list-sessions".to_owned()]],
        &aliases,
    ));
}

#[test]
fn legacy_fallback_detects_alias_after_parse_time_assignment() {
    let groups = vec![vec!["FOO=x".to_owned(), "probe".to_owned()]];
    let aliases = vec!["probe=display-message -p $FOO".to_owned()];

    assert!(queue_uses_server_alias(&groups, &aliases));
    assert_eq!(alias_lookup_command_name(&groups[0]), Some("probe"));
    assert!(is_parse_time_assignment("FOO=x"));
    assert!(is_parse_time_assignment("_FOO=x=y"));
    assert!(!is_parse_time_assignment("1FOO=x"));
    assert!(!is_parse_time_assignment("FOO-BAR=x"));
}

#[test]
fn builtin_cli_assignments_do_not_enter_the_runtime_source_bridge() {
    let alias_only = vec![vec!["FOO=x".to_owned(), "probe".to_owned()]];
    let builtin_only = vec![vec![
        "FOO=x".to_owned(),
        "BAR=y".to_owned(),
        "display-message".to_owned(),
    ]];
    assert!(!queue_has_builtin_after_parse_time_assignment(&alias_only));
    assert!(queue_has_builtin_after_parse_time_assignment(&builtin_only));
    assert!(queue_has_builtin_after_parse_time_assignment(&[vec![
        "FOO=x".to_owned(),
    ]]));
    assert!(queue_requires_direct_parse_for_builtin_assignment(
        &builtin_only,
        &[]
    ));

    let builtin_then_alias = vec![
        vec!["FOO=x".to_owned(), "new-session".to_owned()],
        vec!["probe".to_owned()],
    ];
    let aliases = vec!["probe=display-message -p $FOO".to_owned()];
    assert!(queue_has_builtin_after_parse_time_assignment(
        &builtin_then_alias
    ));
    assert!(!queue_requires_direct_parse_for_builtin_assignment(
        &builtin_then_alias,
        &aliases
    ));
}

#[test]
fn legacy_fallback_keeps_nested_commands_and_builtin_abbreviations_server_owned() {
    let aliases = vec![
        "nested=display-message -p nested".to_owned(),
        "list-sessions=display-message -p override".to_owned(),
    ];

    assert!(!queue_uses_server_alias(
        &[vec![
            "if-shell".to_owned(),
            "-F".to_owned(),
            "1".to_owned(),
            "nested".to_owned(),
        ]],
        &aliases,
    ));
    assert!(!queue_uses_server_alias(
        &[vec!["list-sess".to_owned()]],
        &aliases,
    ));
}

#[test]
fn alias_fallback_errors_strip_synthetic_source_file_prefix() {
    assert_eq!(
        strip_source_file_stdin_line_prefix("-:1: unknown command: nope"),
        Some("unknown command: nope")
    );
    assert_eq!(
        strip_source_file_stdin_line_prefix("unknown command: nope"),
        None
    );
}
