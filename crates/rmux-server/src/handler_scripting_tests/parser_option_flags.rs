use super::*;

use crate::handler::scripting_support::parse_request_from_parts;
use rmux_core::{ManagedSessionStore, OptionStore};
use rmux_proto::RmuxError;

fn parser_fixture() -> (ManagedSessionStore, TargetFindContext) {
    let alpha = session_name("alpha");
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("parser fixture session");
    let find_context =
        TargetFindContext::from_target(Target::Pane(PaneTarget::with_window(alpha, 0, 0)));
    (sessions, find_context)
}

fn parse_server_request(
    command: &str,
    arguments: &[&str],
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    parse_request_from_parts(
        command.to_owned(),
        arguments
            .iter()
            .map(|argument| (*argument).to_owned())
            .collect(),
        None,
        sessions,
        &OptionStore::default(),
        find_context,
    )
}

#[test]
fn server_new_positional_parsers_reject_unknown_option_shapes() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments, unknown) in [
        ("set-option", &["-x", "value"][..], "-x"),
        ("set-option", &["--bogus", "value"][..], "--bogus"),
        ("set-option", &["-gx", "value"][..], "-gx"),
        ("set-window-option", &["-x", "value"][..], "-x"),
        ("set-window-option", &["--bogus", "value"][..], "--bogus"),
        ("set-window-option", &["-gx", "value"][..], "-gx"),
        ("show-options", &["-x"][..], "-x"),
        ("show-options", &["--bogus"][..], "--bogus"),
        ("show-options", &["-gx"][..], "-gx"),
        ("show-window-options", &["-x"][..], "-x"),
        ("show-window-options", &["--bogus"][..], "--bogus"),
        ("show-window-options", &["-gx"][..], "-gx"),
    ] {
        let error = parse_server_request(command, arguments, &sessions, &find_context)
            .expect_err("unknown option before a positional operand must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag {unknown}")),
            "{command} absorbed {unknown} as a positional operand"
        );
    }
}

#[test]
fn migrated_key_parsers_report_tmux_first_invalid_flag() {
    let (sessions, find_context) = parser_fixture();

    // tmux 3.7b was measured directly for every cell in this table.
    for (command, arguments, expected) in [
        ("unbind-key", &["-x"][..], "unknown flag -x"),
        ("unbind-key", &["--bogus"][..], "invalid flag --"),
        ("unbind-key", &["-nx"][..], "unknown flag -x"),
        ("list-keys", &["-x"][..], "unknown flag -x"),
        ("list-keys", &["--bogus"][..], "invalid flag --"),
        ("list-keys", &["-ax"][..], "unknown flag -x"),
    ] {
        let error = parse_server_request(command, arguments, &sessions, &find_context)
            .expect_err("invalid compact option must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: {expected}")),
            "{command} did not report tmux's first invalid flag for {arguments:?}"
        );
    }
}

#[test]
fn server_new_positional_parsers_preserve_legitimate_flags() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments) in [
        ("set-option", &["-g", "@r3-global", "value"][..]),
        ("set-option", &["-s", "@r3-server", "value"][..]),
        ("set-option", &["-w", "@r3-window", "value"][..]),
        ("set-option", &["-p", "@r3-pane", "value"][..]),
        ("set-option", &["-q", "@r3-quiet", "value"][..]),
        ("set-option", &["-a", "@r3-append", "value"][..]),
        ("set-option", &["-F", "@r3-format", "#{session_name}"][..]),
        ("set-option", &["-o", "@r3-only-unset", "value"][..]),
        ("set-option", &["-u", "@r3-unset"][..]),
        ("set-option", &["-U", "@r3-unset-panes"][..]),
        ("set-option", &["-t", "alpha:0", "@r3-target", "value"][..]),
        ("set-option", &["-gF", "@r3-cluster", "value"][..]),
        (
            "set-window-option",
            &["-g", "@r3-window-global", "value"][..],
        ),
        (
            "set-window-option",
            &["-q", "@r3-window-quiet", "value"][..],
        ),
        (
            "set-window-option",
            &["-a", "@r3-window-append", "value"][..],
        ),
        (
            "set-window-option",
            &["-F", "@r3-window-format", "#{session_name}"][..],
        ),
        (
            "set-window-option",
            &["-o", "@r3-window-only-unset", "value"][..],
        ),
        ("set-window-option", &["-u", "@r3-window-unset"][..]),
        (
            "set-window-option",
            &["-t", "alpha:0", "@r3-window-target", "value"][..],
        ),
        (
            "set-window-option",
            &["-gF", "@r3-window-cluster", "value"][..],
        ),
        ("show-options", &["-g"][..]),
        ("show-options", &["-s"][..]),
        ("show-options", &["-w"][..]),
        ("show-options", &["-p"][..]),
        ("show-options", &["-v"][..]),
        ("show-options", &["-A"][..]),
        ("show-options", &["-H"][..]),
        ("show-options", &["-q"][..]),
        ("show-options", &["-t", "alpha:0"][..]),
        ("show-options", &["-gv", "status-right"][..]),
        ("show-window-options", &["-g"][..]),
        ("show-window-options", &["-v"][..]),
        ("show-window-options", &["-t", "alpha:0"][..]),
        (
            "show-window-options",
            &["-gv", "window-status-current-format"][..],
        ),
        ("unbind-key", &["-a"][..]),
        ("unbind-key", &["-n", "C-z"][..]),
        ("unbind-key", &["-q", "C-z"][..]),
        ("unbind-key", &["-T", "root", "C-z"][..]),
        ("unbind-key", &["-nq", "C-z"][..]),
        ("list-keys", &["-1"][..]),
        ("list-keys", &["-a"][..]),
        ("list-keys", &["-N"][..]),
        ("list-keys", &["-r"][..]),
        ("list-keys", &["-F", "#{key}"][..]),
        ("list-keys", &["-O", "key"][..]),
        ("list-keys", &["-P", "C-b"][..]),
        ("list-keys", &["-T", "root"][..]),
        ("list-keys", &["-aN"][..]),
    ] {
        parse_server_request(command, arguments, &sessions, &find_context).unwrap_or_else(
            |error| panic!("{command} rejected legitimate arguments {arguments:?}: {error}"),
        );
    }
}

#[test]
fn server_new_positional_parsers_preserve_missing_value_errors() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments, expected) in [
        ("set-option", &["-t"][..], "missing -t target"),
        ("set-window-option", &["-t"][..], "missing -t target"),
        ("show-options", &["-t"][..], "missing -t target"),
        ("show-window-options", &["-t"][..], "missing -t target"),
        ("unbind-key", &["-T"][..], "missing -T key-table"),
        ("list-keys", &["-F"][..], "missing -F format"),
    ] {
        assert_eq!(
            parse_server_request(command, arguments, &sessions, &find_context)
                .expect_err("known value-taking option without its value must fail"),
            RmuxError::Server(expected.to_owned())
        );
    }
}

#[test]
fn server_new_positional_parsers_preserve_explicit_separator() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments) in [
        ("set-option", &["--", "@r3-separator", "value"][..]),
        (
            "set-window-option",
            &["--", "@r3-window-separator", "value"][..],
        ),
        ("show-options", &["--", "status-right"][..]),
        (
            "show-window-options",
            &["--", "window-status-current-format"][..],
        ),
        ("unbind-key", &["--", "C-z"][..]),
        ("list-keys", &["--", "c"][..]),
    ] {
        parse_server_request(command, arguments, &sessions, &find_context).unwrap_or_else(
            |error| panic!("{command} rejected explicit -- arguments {arguments:?}: {error}"),
        );
    }
}
