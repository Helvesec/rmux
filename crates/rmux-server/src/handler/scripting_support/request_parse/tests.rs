use super::*;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    PaneTarget, ScopeSelector, SessionName, SetEnvironmentMode, TerminalSize, WindowTarget,
};

fn parse_request(command: &str, args: &[&str]) -> Request {
    parse_request_with_sessions(command, args, &ManagedSessionStore::default())
}

fn parse_request_with_sessions(
    command: &str,
    args: &[&str],
    sessions: &ManagedSessionStore,
) -> Request {
    parse_request_result(command, args, sessions).expect("request parses")
}

fn parse_request_result(
    command: &str,
    args: &[&str],
    sessions: &ManagedSessionStore,
) -> Result<Request, RmuxError> {
    parse_request_from_parts(
        command.to_owned(),
        args.iter().map(|arg| (*arg).to_owned()).collect(),
        None,
        sessions,
        &OptionStore::default(),
        &TargetFindContext::new(None),
    )
}

fn select_layout_sessions() -> (ManagedSessionStore, SessionName) {
    let session_name = SessionName::new("alpha").expect("valid session name");
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("session creation succeeds");
    (sessions, session_name)
}

fn parse_select_layout_invocation(
    command: &str,
    sessions: &ManagedSessionStore,
    session_name: &SessionName,
) -> Result<QueueInvocation, RmuxError> {
    let parsed = CommandParser::new()
        .parse(command)
        .expect("select-layout command parses");
    assert_eq!(parsed.commands().len(), 1);
    parse_queue_invocation(
        parsed.commands()[0].clone(),
        None,
        sessions,
        &OptionStore::default(),
        &TargetFindContext::new(Some(Target::Window(WindowTarget::with_window(
            session_name.clone(),
            0,
        )))),
        None,
        None,
    )
}

fn parse_select_layout_request(
    command: &str,
    sessions: &ManagedSessionStore,
    session_name: &SessionName,
) -> Request {
    let invocation = parse_select_layout_invocation(command, sessions, session_name)
        .unwrap_or_else(|error| panic!("{command} should parse: {error}"));
    let QueueInvocation::Request(request) = invocation else {
        panic!("{command} should produce a request, got {invocation:?}");
    };
    request
}

fn parse_targeted_queue_request(
    command: &str,
    sessions: &ManagedSessionStore,
    current_target: Target,
) -> Request {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("{command} should parse: {error}"));
    assert_eq!(parsed.commands().len(), 1);
    let invocation = parse_queue_invocation(
        parsed.commands()[0].clone(),
        None,
        sessions,
        &OptionStore::default(),
        &TargetFindContext::from_target(current_target),
        None,
        None,
    )
    .unwrap_or_else(|error| panic!("{command} queue target should resolve: {error}"));
    let QueueInvocation::Request(request) = invocation else {
        panic!("{command} should produce a request, got {invocation:?}");
    };
    request
}

#[test]
fn queued_target_metadata_resolves_window_relative_pane_id_and_session_targets() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(alpha.clone(), TerminalSize { cols: 80, rows: 24 })
        .expect("session creation succeeds");
    let (logs_index, _) = sessions
        .session_setup(&alpha)
        .expect("alpha exists")
        .create_window(TerminalSize { cols: 80, rows: 24 })
        .expect("logs window creation succeeds");
    assert_eq!(logs_index, 1);
    let active_pane_index = {
        let mut session = sessions.session_setup(&alpha).expect("alpha exists");
        session
            .rename_window(logs_index, "logs".to_owned())
            .expect("logs window rename succeeds");
        session
            .split_active_pane()
            .expect("active pane split succeeds")
    };
    let active_pane = PaneTarget::with_window(alpha.clone(), 0, active_pane_index);
    let logs_pane_id = sessions
        .session(&alpha)
        .and_then(|session| session.window_at(logs_index))
        .and_then(|window| window.pane(0))
        .map(|pane| pane.id().to_string())
        .expect("logs pane id exists");
    let current = Target::Pane(PaneTarget::with_window(alpha.clone(), 0, 0));

    let Request::ResizeWindow(resize) =
        parse_targeted_queue_request("resize-window -t logs -x 91", &sessions, current.clone())
    else {
        panic!("resize-window should produce a resize request");
    };
    assert_eq!(
        resize.target,
        WindowTarget::with_window(alpha.clone(), logs_index)
    );

    let Request::ShowOptions(show) = parse_targeted_queue_request(
        &format!("show-window-options -v -t {logs_pane_id} automatic-rename"),
        &sessions,
        current.clone(),
    ) else {
        panic!("show-window-options should produce a show-options request");
    };
    assert_eq!(
        show.scope,
        rmux_proto::OptionScopeSelector::Window(WindowTarget::with_window(
            alpha.clone(),
            logs_index,
        ))
    );

    let Request::UnlinkWindow(unlink) =
        parse_targeted_queue_request("unlink-window -t :+", &sessions, current.clone())
    else {
        panic!("unlink-window should produce an unlink request");
    };
    assert_eq!(
        unlink.target,
        WindowTarget::with_window(alpha.clone(), logs_index)
    );

    let Request::SendPrefix(send_prefix) =
        parse_targeted_queue_request("send-prefix -talpha", &sessions, current)
    else {
        panic!("send-prefix should produce a send-prefix request");
    };
    assert_eq!(send_prefix.target, Some(active_pane));
}

#[test]
fn queued_select_layout_navigation_uses_existing_layout_cycle_requests() {
    let (sessions, session_name) = select_layout_sessions();

    let next =
        parse_select_layout_invocation("select-layout -n -t alpha:0", &sessions, &session_name)
            .expect("next layout parses");
    let QueueInvocation::Request(Request::NextLayout(request)) = next else {
        panic!("expected next-layout request, got {next:?}");
    };
    assert_eq!(
        request.target,
        WindowTarget::with_window(session_name.clone(), 0)
    );

    let previous =
        parse_select_layout_invocation("select-layout -ptalpha:0", &sessions, &session_name)
            .expect("previous layout compact flags parse");
    let QueueInvocation::Request(Request::PreviousLayout(request)) = previous else {
        panic!("expected previous-layout request, got {previous:?}");
    };
    assert_eq!(request.target, WindowTarget::with_window(session_name, 0));
}

#[test]
fn queued_select_layout_without_layout_is_a_validated_noop() {
    let (sessions, session_name) = select_layout_sessions();

    for command in ["select-layout", "select-layout -t alpha:0"] {
        let invocation = parse_select_layout_invocation(command, &sessions, &session_name)
            .unwrap_or_else(|error| panic!("{command} should parse: {error}"));
        assert!(
            matches!(invocation, QueueInvocation::NoOp),
            "{command} should be a no-op, got {invocation:?}"
        );
    }

    let error =
        parse_select_layout_invocation("select-layout -t missing:0", &sessions, &session_name)
            .expect_err("a no-op must still validate its target");
    assert!(
        error.to_string().contains("can't find session: missing"),
        "unexpected missing target error: {error}"
    );
}

#[test]
fn queued_select_layout_uses_tmux_mode_priority_and_operand_semantics() {
    let (sessions, session_name) = select_layout_sessions();
    assert!(matches!(
        parse_select_layout_request("select-layout -E -t alpha:0", &sessions, &session_name),
        Request::SpreadLayout(_)
    ));
    assert!(matches!(
        parse_select_layout_request("select-layout -o -t alpha:0", &sessions, &session_name),
        Request::SelectOldLayout(_)
    ));
    assert!(matches!(
        parse_select_layout_request(
            "select-layout -t alpha:0 even-horizontal",
            &sessions,
            &session_name
        ),
        Request::SelectLayout(_)
    ));
    assert!(matches!(
        parse_select_layout_request(
            "select-layout -t alpha:0 '89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}'",
            &sessions,
            &session_name
        ),
        Request::SelectCustomLayout(_)
    ));

    for command in [
        "select-layout -n -p -t alpha:0",
        "select-layout -E -n -t alpha:0",
        "selectl -nE -t alpha:0",
        "select-layout -Enop -t alpha:0 tiled",
    ] {
        assert!(
            matches!(
                parse_select_layout_request(command, &sessions, &session_name),
                Request::NextLayout(_)
            ),
            "-n must govern {command}"
        );
    }
    assert!(matches!(
        parse_select_layout_request("select-layout -Ep -t alpha:0", &sessions, &session_name),
        Request::PreviousLayout(_)
    ));
    assert!(matches!(
        parse_select_layout_request("select-layout -Eo -t alpha:0", &sessions, &session_name),
        Request::SpreadLayout(_)
    ));

    let Request::SelectCustomLayout(custom) = parse_select_layout_request(
        "select-layout -o -t alpha:0 tiled",
        &sessions,
        &session_name,
    ) else {
        panic!("-o with an operand must defer strict custom-layout validation");
    };
    assert_eq!(custom.layout, "tiled");
}

#[test]
fn command_tail_parsers_reject_unknown_options_before_positionals() {
    let cases = [
        ("run-shell", &["-Q", "true"][..]),
        ("if-shell", &["-Q", "true", "display-message ok"][..]),
        ("wait-for", &["-Q"][..]),
        ("split-window", &["-Q", "true"][..]),
        ("pipe-pane", &["-Q", "true"][..]),
        ("respawn-pane", &["-Q", "true"][..]),
        ("bind-key", &["-Q", "x", "display-message ok"][..]),
        ("display-panes", &["-Q", "select-pane -t %%"][..]),
    ];

    for (command, arguments) in cases {
        let error = parse_request_result(command, arguments, &ManagedSessionStore::default())
            .expect_err("unknown option before positional tail must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag -Q")),
            "{command} accepted an unknown option as a positional tail"
        );
    }
}

#[test]
fn command_tail_parsers_reject_long_and_mixed_unknown_options() {
    for (command, arguments, expected_flag) in [
        ("run-shell", &["-bQ", "true"][..], "-bQ"),
        (
            "if-shell",
            &["-bQ", "true", "display-message ok"][..],
            "-bQ",
        ),
        ("wait-for", &["--bogus"][..], "--bogus"),
        ("split-window", &["-hQ", "true"][..], "-Q"),
        ("pipe-pane", &["-IQ", "true"][..], "-IQ"),
        ("respawn-pane", &["-kQ", "true"][..], "-kQ"),
        ("bind-key", &["-nQ", "x", "display-message ok"][..], "-Q"),
        ("display-panes", &["-bQ", "select-pane -t %%"][..], "-Q"),
    ] {
        let error = parse_request_result(command, arguments, &ManagedSessionStore::default())
            .expect_err("mixed or long unknown option must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag {expected_flag}"))
        );
    }
}

#[test]
fn explicit_separator_allows_dash_prefixed_positional_tails() {
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(
            SessionName::new("alpha").expect("valid session name"),
            TerminalSize { cols: 80, rows: 24 },
        )
        .expect("session create succeeds");

    for (command, arguments) in [
        ("run-shell", &["--", "-Q"][..]),
        ("if-shell", &["--", "-Q", "display-message ok"][..]),
        ("wait-for", &["--", "-Q"][..]),
        ("split-window", &["--", "-Q"][..]),
        ("pipe-pane", &["--", "-Q"][..]),
        ("respawn-pane", &["--", "-Q"][..]),
        ("bind-key", &["--", "-Q", "display-message ok"][..]),
        ("display-panes", &["--", "-Q"][..]),
    ] {
        parse_request_result(command, arguments, &sessions)
            .unwrap_or_else(|error| panic!("{command} rejected -- -Q: {error}"));
    }
}

fn assert_mouse_target(target: Option<&PaneTarget>) {
    let target = target.expect("target should parse from -t=");
    assert_eq!(target.session_name().as_str(), "=");
    assert_eq!(target.window_index(), 0);
    assert_eq!(target.pane_index(), 0);
}

#[test]
fn default_mouse_binding_copy_mode_compact_target_parses() {
    let Request::CopyMode(request) = parse_request("copy-mode", &["-Ht="]) else {
        panic!("copy-mode -Ht= should parse as CopyMode");
    };

    assert!(request.hide_position);
    assert_mouse_target(request.target.as_ref());
}

#[test]
fn default_mouse_binding_send_keys_compact_target_parses() {
    let Request::SendKeysExt(request) = parse_request("send-keys", &["-Xt=", "select-word"]) else {
        panic!("send -Xt= should parse as extended send-keys");
    };

    assert!(request.copy_mode_command);
    assert_mouse_target(request.target.as_ref());
    assert_eq!(request.keys, vec!["select-word"]);
}

#[test]
fn set_environment_accepts_compact_global_unset_flags() {
    let Request::SetEnvironment(request) = parse_request("set-environment", &["-gu", "AUDIT_VAR"])
    else {
        panic!("set-environment -gu should parse as SetEnvironment");
    };

    assert_eq!(request.scope, ScopeSelector::Global);
    assert_eq!(request.mode, Some(SetEnvironmentMode::Unset));
    assert_eq!(request.name, "AUDIT_VAR");
    assert!(request.value.is_empty());
}

#[test]
fn compact_short_options_reach_server_request_parsers_for_boolean_flags() {
    // Measured against tmux 3.7b on 2026-07-15: `capture-pane -ep`,
    // `show-messages -JT`, `show-environment -gs`, `attach-session -dr`,
    // and `rotate-window -DZ` all pass command-option parsing.
    let mut sessions = ManagedSessionStore::new();
    sessions
        .create_session(
            SessionName::new("alpha").expect("valid session name"),
            TerminalSize { cols: 80, rows: 24 },
        )
        .expect("session create succeeds");

    let Request::CapturePane(request) =
        parse_request_with_sessions("capture-pane", &["-ep", "-t", "alpha:0.0"], &sessions)
    else {
        panic!("capture-pane compact flags should parse as CapturePane");
    };
    assert!(request.escape_ansi);
    assert!(request.print);

    let Request::ShowMessages(request) = parse_request("show-messages", &["-JT"]) else {
        panic!("show-messages compact flags should parse as ShowMessages");
    };
    assert!(request.jobs);
    assert!(request.terminals);

    let Request::ShowEnvironment(request) = parse_request("show-environment", &["-gs", "PATH"])
    else {
        panic!("show-environment compact flags should parse as ShowEnvironment");
    };
    assert_eq!(request.scope, ScopeSelector::Global);
    assert!(request.shell_format);

    let Request::AttachSessionExt2(request) = parse_request("attach-session", &["-dr"]) else {
        panic!("attach-session compact flags should parse as AttachSessionExt2");
    };
    assert!(request.detach_other_clients);
    assert!(request.read_only);

    let Request::RotateWindow(request) =
        parse_request_with_sessions("rotate-window", &["-DZ", "-t", "alpha:0"], &sessions)
    else {
        panic!("rotate-window compact flags should parse as RotateWindow");
    };
    assert_eq!(request.direction, rmux_proto::RotateWindowDirection::Down);
    assert!(request.restore_zoom);
}

#[test]
fn compact_short_options_reach_server_request_parsers_for_value_flags() {
    // Measured against tmux 3.7b on 2026-07-15: run-shell accepts values
    // attached to both `-c` and `-t`.
    let Request::RunShell(request) = parse_request(
        "run-shell",
        &["-ccompact-dir", "-talpha:0.0", "printf compact"],
    ) else {
        panic!("run-shell compact value flags should parse as RunShell");
    };

    assert_eq!(
        request.start_directory.as_deref(),
        Some(std::path::Path::new("compact-dir"))
    );
    assert_eq!(
        request.target.as_ref().map(ToString::to_string).as_deref(),
        Some("alpha:0.0")
    );
    assert_eq!(request.command, "printf compact");
}

#[test]
fn compact_hidden_buffer_flags_reach_server_request_parsers() {
    let Request::LoadBuffer(request) = parse_request("load-buffer", &["-wbclip", "/tmp/input"])
    else {
        panic!("load-buffer hidden compact flags should parse as LoadBuffer");
    };
    assert!(request.set_clipboard);
    assert_eq!(request.name.as_deref(), Some("clip"));
    assert_eq!(request.path, "/tmp/input");

    let Request::ListBuffers(request) = parse_request("list-buffers", &["-rF#{buffer_name}"])
    else {
        panic!("list-buffers hidden compact flag should parse as ListBuffers");
    };
    assert!(request.reversed);
    assert_eq!(request.format.as_deref(), Some("#{buffer_name}"));
}

#[test]
fn set_environment_compact_target_consumes_only_its_value() {
    let Request::SetEnvironment(request) =
        parse_request("set-environment", &["-Fhtalpha", "AUDIT_VAR", "#{version}"])
    else {
        panic!("set-environment compact target should parse as SetEnvironment");
    };

    let ScopeSelector::Session(scope) = request.scope else {
        panic!("compact -t should select session scope");
    };
    assert_eq!(scope.as_str(), "alpha");
    assert!(request.format);
    assert!(request.hidden);
    assert_eq!(request.name, "AUDIT_VAR");
    assert_eq!(request.value, "#{version}");
}

#[test]
fn set_environment_unset_takes_precedence_over_clear() {
    // Measured against the pinned tmux 3.7b oracle on 2026-07-14:
    // compact and separated `-r -u`/`-u -r` all use `-u` semantics.
    for arguments in [
        vec!["-gru".to_owned(), "AUDIT_VAR".to_owned()],
        vec!["-gur".to_owned(), "AUDIT_VAR".to_owned()],
        vec![
            "-g".to_owned(),
            "-r".to_owned(),
            "-u".to_owned(),
            "AUDIT_VAR".to_owned(),
        ],
        vec![
            "-g".to_owned(),
            "-u".to_owned(),
            "-r".to_owned(),
            "AUDIT_VAR".to_owned(),
        ],
    ] {
        let Request::SetEnvironment(request) = parse_request_from_parts(
            "set-environment".to_owned(),
            arguments,
            None,
            &ManagedSessionStore::default(),
            &OptionStore::default(),
            &TargetFindContext::new(None),
        )
        .expect("set-environment accepts combined mode flags") else {
            panic!("expected set-environment request");
        };

        assert_eq!(request.mode, Some(SetEnvironmentMode::Unset));
    }
}
