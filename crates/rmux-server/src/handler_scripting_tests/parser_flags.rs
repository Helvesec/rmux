use super::*;

use crate::handler::scripting_support::parse_request_from_parts;
use rmux_core::{ManagedSessionStore, OptionStore};
use rmux_proto::RmuxError;

const UNKNOWN_FLAG_COMMANDS: [(&str, &str); 13] = [
    ("set-buffer", "set-buffer -x payload"),
    ("rename-session", "rename-session -x"),
    ("rename-window", "rename-window -x"),
    ("display-message", "display-message -p -x"),
    ("send-keys", "send-keys -x C-c"),
    (
        "set-environment",
        "set-environment -g -x parser-flag-poison",
    ),
    ("set-option", "set-option -x value"),
    ("set-window-option", "set-window-option -x value"),
    ("show-options", "show-options -x"),
    ("show-window-options", "show-window-options -x"),
    ("unbind-key", "unbind-key -x"),
    ("list-keys", "list-keys -x"),
    ("select-layout", "select-layout -x"),
];

const DAEMON_TEST_STACK_SIZE: usize = 8 * 1024 * 1024;
const NEW_UNKNOWN_FLAG_COMMAND_START: usize = 6;

fn run_on_daemon_test_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + 'static,
{
    let worker = std::thread::Builder::new()
        .name("parser-flags-test".to_owned())
        .stack_size(DAEMON_TEST_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("parser-flags test runtime should build");
            runtime.block_on(test());
        })
        .expect("parser-flags test worker should spawn");
    if let Err(panic) = worker.join() {
        std::panic::resume_unwind(panic);
    }
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

#[test]
fn server_tail_parsers_reject_unknown_flags_before_positionals() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments) in [
        ("send-keys", &["-x", "C-c"][..]),
        ("rename-session", &["-x"][..]),
        ("rename-window", &["-x"][..]),
        ("set-buffer", &["-x", "payload"][..]),
        ("display-message", &["-p", "-x"][..]),
    ] {
        let error = parse_server_request(command, arguments, &sessions, &find_context)
            .expect_err("unknown flag before positional must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag -x")),
            "{command} absorbed -x as a positional operand"
        );
    }
}

#[test]
fn compact_flag_clusters_report_tmux_first_unsupported_flag() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments, expected_flag) in [
        ("select-pane", &["-value"][..], "-v"),
        ("resize-pane", &["-value"][..], "-v"),
        ("list-panes", &["-value"][..], "-v"),
        ("kill-pane", &["-value"][..], "-v"),
        ("swap-pane", &["-value"][..], "-v"),
        ("list-windows", &["-value"][..], "-v"),
        ("new-session", &["-dvalue"][..], "-v"),
        ("send-keys", &["-Xvalue"][..], "-v"),
        ("split-window", &["-dvalue"][..], "-a"),
    ] {
        let error = parse_server_request(command, arguments, &sessions, &find_context)
            .expect_err("unsupported flag in compact cluster must fail");
        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag {expected_flag}")),
            "{command} did not report tmux's first unsupported compact flag"
        );
    }
}

#[test]
fn server_set_environment_rejects_unknown_option_shapes() {
    let (sessions, find_context) = parser_fixture();

    for (arguments, expected) in [
        (
            &["-g", "-x", "short-poison"][..],
            "command set-environment: unknown flag -x",
        ),
        (
            &["-g", "--bogus", "long-poison"][..],
            "command set-environment: invalid flag --",
        ),
        (
            &["-g", "-Fz", "cluster-poison"][..],
            "command set-environment: unknown flag -z",
        ),
    ] {
        let error = parse_server_request("set-environment", arguments, &sessions, &find_context)
            .expect_err("unknown set-environment option must fail");
        assert_eq!(
            error,
            RmuxError::Server(expected.to_owned()),
            "set-environment did not match tmux for {arguments:?}"
        );
    }
}

#[test]
fn server_set_environment_preserves_options_and_terminator() {
    let (sessions, find_context) = parser_fixture();

    for arguments in [
        &["-g", "GLOBAL_NAME", "value"][..],
        &["-F", "FORMAT_NAME", "#{session_name}"][..],
        &["-h", "HIDDEN_NAME", "value"][..],
        &["-r", "CLEAR_NAME"][..],
        &["-u", "UNSET_NAME"][..],
        &["-t", "alpha", "TARGET_NAME", "value"][..],
        &["-Fgh", "COMPACT_NAME", "value"][..],
        &["-talpha", "COMPACT_TARGET_NAME", "value"][..],
        &["-g", "--", "-DASH_NAME", "value"][..],
    ] {
        parse_server_request("set-environment", arguments, &sessions, &find_context)
            .unwrap_or_else(|error| {
                panic!("set-environment rejected legitimate arguments {arguments:?}: {error}")
            });
    }

    assert_eq!(
        parse_server_request("set-environment", &["-g", "-t"], &sessions, &find_context,)
            .expect_err("-t without a target must fail"),
        RmuxError::Server("missing -t target".to_owned())
    );
    assert_eq!(
        parse_server_request("set-environment", &["--"], &sessions, &find_context)
            .expect_err("-- without a variable name must fail"),
        RmuxError::Server("missing set-environment name".to_owned())
    );
}

#[test]
fn server_tail_parsers_preserve_explicit_dash_prefixed_positionals() {
    let (sessions, find_context) = parser_fixture();

    for (command, arguments) in [
        ("send-keys", &["--", "-x"][..]),
        ("rename-session", &["--", "-x"][..]),
        ("rename-window", &["--", "-x"][..]),
        ("set-buffer", &["--", "-x"][..]),
        ("display-message", &["-p", "--", "-x"][..]),
    ] {
        parse_server_request(command, arguments, &sessions, &find_context)
            .unwrap_or_else(|error| panic!("{command} rejected -- -x: {error}"));
    }
}

async fn create_stable_session(handler: &RequestHandler, name: &SessionName) -> u32 {
    create_background_identity_session(handler, name.clone()).await;
    let rename = handler
        .handle(Request::RenameWindow(rmux_proto::RenameWindowRequest {
            target: WindowTarget::with_window(name.clone(), 0),
            name: "stable-window".to_owned(),
        }))
        .await;
    assert!(matches!(rename, Response::RenameWindow(_)), "{rename:?}");

    // This fixture asserts that a refused flag writes nothing to the pane, so
    // the pane must not report focus edges of its own into the same capture.
    handler
        .disable_pane_focus_reports_for_test(&PaneTarget::with_window(name.clone(), 0, 0))
        .await;

    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(name)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.pane(0))
        .map(|pane| pane.id().as_u32())
        .expect("stable pane exists");
    state.start_pane_input_capture_for_test(&PaneTarget::with_window(name.clone(), 0, 0));
    pane_id
}

async fn assert_stable_session(handler: &RequestHandler, name: &SessionName, pane_id: u32) {
    let state = handler.state.lock().await;
    let session = state
        .sessions
        .session(name)
        .expect("unknown flag must not rename the session");
    let window = session
        .window_at(0)
        .expect("unknown flag must not remove the window");
    assert_eq!(
        window.name(),
        Some("stable-window"),
        "unknown flag must not rename the window"
    );
    assert_eq!(
        window
            .pane(0)
            .expect("unknown flag must not remove the pane")
            .id()
            .as_u32(),
        pane_id
    );
    assert!(
        state.buffers.is_empty(),
        "unknown flag must not create its own buffer or run a buffer canary"
    );
    assert_eq!(
        state.pane_input_capture_for_test(&PaneTarget::with_window(name.clone(), 0, 0)),
        Some(Vec::new()),
        "unknown flag must not write input to the pane"
    );
}

async fn assert_named_buffer_absent(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some(name.to_owned()),
        }))
        .await;
    assert!(
        matches!(response, Response::Error(_)),
        "buffer {name:?} proves a rejected command tail still executed: {response:?}"
    );
}

async fn assert_global_environment_absent(handler: &RequestHandler, name: &str) {
    let state = handler.state.lock().await;
    let entries = state
        .environment
        .show_environment_entries(&ScopeSelector::Global, true, None)
        .unwrap_or_else(|error| panic!("show global environment {name:?}: {error}"));
    assert!(
        entries.iter().all(|entry| entry.name != name),
        "unknown flag mutated global environment {name:?}: {entries:?}"
    );
}

async fn assert_control_mode_case(index: usize, command: &str, invalid: &str) {
    let handler = RequestHandler::new();
    let alpha = session_name("control-parser-flags");
    let pane_id = create_stable_session(&handler, &alpha).await;
    let requester_pid = 63_000 + index as u32;
    let (_control_id, _control_events) =
        register_control_for_session(&handler, requester_pid, alpha.clone()).await;
    let canary = format!("control-unknown-{index}");
    let commands = handler
        .parse_control_commands(&format!("{invalid} ; set-buffer -b {canary} must-not-run"))
        .await
        .expect("control command syntax parses");

    let result = handler
        .execute_control_commands(requester_pid, commands)
        .await;

    assert_eq!(
        result.error,
        Some(RmuxError::Server(format!(
            "command {command}: unknown flag -x"
        ))),
        "{invalid} did not fail closed in control-mode"
    );
    assert_named_buffer_absent(&handler, &canary).await;
    assert_global_environment_absent(&handler, "-x").await;
    assert_stable_session(&handler, &alpha, pane_id).await;
}

#[test]
fn control_mode_rejects_unknown_flags_without_follow_on_effects() {
    run_on_daemon_test_stack(control_mode_rejects_unknown_flags_body);
}

async fn control_mode_rejects_unknown_flags_body() {
    for (index, (command, invalid)) in UNKNOWN_FLAG_COMMANDS.into_iter().enumerate() {
        Box::pin(assert_control_mode_case(index, command, invalid)).await;
    }
}

async fn assert_explicit_source_file_case(index: usize, command: &str, invalid: &str) {
    let handler = RequestHandler::new();
    let alpha = session_name("source-parser-flags");
    let pane_id = create_stable_session(&handler, &alpha).await;
    let root = temp_root(&format!("w13-h14-source-{index}"));
    let config = root.join("main.conf");
    let canary = format!("source-unknown-{index}");
    write_config(
        &config,
        &format!("{invalid}\nset-buffer -b {canary} must-not-run\n"),
    );

    let response = handler
        .handle(source_file_request(
            vec!["main.conf".to_owned()],
            Some(root.to_path_buf()),
        ))
        .await;

    assert_eq!(
        source_file_stdout_failure(response),
        format!(
            "{}:1: command {command}: unknown flag -x\n",
            config.display()
        ),
        "{invalid} did not fail closed in source-file"
    );
    assert_named_buffer_absent(&handler, &canary).await;
    assert_global_environment_absent(&handler, "-x").await;
    assert_stable_session(&handler, &alpha, pane_id).await;
    fs::remove_dir_all(root).expect("remove parser-flag source root");
}

#[test]
fn explicit_source_file_rejects_unknown_flags_without_effects() {
    run_on_daemon_test_stack(explicit_source_file_rejects_unknown_flags_body);
}

async fn explicit_source_file_rejects_unknown_flags_body() {
    for (index, (command, invalid)) in UNKNOWN_FLAG_COMMANDS.into_iter().enumerate() {
        Box::pin(assert_explicit_source_file_case(index, command, invalid)).await;
    }
}

async fn assert_startup_config_case(index: usize, command: &str, invalid: &str) {
    let handler = RequestHandler::new();
    let alpha = session_name("startup-parser-flags");
    let pane_id = create_stable_session(&handler, &alpha).await;
    let root = temp_root(&format!("w13-h14-startup-{index}"));
    let config_path = root.join("startup.conf");
    let canary = format!("startup-unknown-{index}");
    write_config(
        &config_path,
        &format!("{invalid}\nset-buffer -b {canary} must-not-run\n"),
    );
    let config = crate::DaemonConfig::new(root.join("rmux.sock")).with_config_files(
        vec![PathBuf::from("startup.conf")],
        false,
        Some(root.to_path_buf()),
    );

    handler
        .load_startup_config(config.config_load().clone())
        .await;

    let errors = handler.startup_config_errors.lock().await;
    let rendered = errors
        .first()
        .unwrap_or_else(|| panic!("{invalid} should record a startup config error"))
        .to_string();
    assert!(
        rendered.contains(&format!(
            "startup.conf:1: command {command}: unknown flag -x"
        )),
        "unexpected startup error for {invalid}: {rendered}"
    );
    drop(errors);
    assert_named_buffer_absent(&handler, &canary).await;
    assert_global_environment_absent(&handler, "-x").await;
    assert_stable_session(&handler, &alpha, pane_id).await;
    fs::remove_dir_all(root).expect("remove parser-flag startup root");
}

#[test]
fn startup_config_rejects_unknown_flags_without_effects() {
    run_on_daemon_test_stack(startup_config_rejects_unknown_flags_body);
}

async fn startup_config_rejects_unknown_flags_body() {
    for (index, (command, invalid)) in UNKNOWN_FLAG_COMMANDS.into_iter().enumerate() {
        Box::pin(assert_startup_config_case(index, command, invalid)).await;
    }
}

async fn assert_bind_key_case(index: usize, command: &str, invalid: &str) {
    let handler = RequestHandler::new();
    let alpha = session_name("binding-parser-flags");
    let pane_id = create_stable_session(&handler, &alpha).await;
    let key = char::from(b'A' + index as u8).to_string();
    let canary = format!("binding-unknown-{index}");
    let binding = CommandParser::new()
        .parse(&format!(
            "bind-key -T root {key} {{ {invalid} ; \
             set-buffer -b {canary} must-not-run }}"
        ))
        .expect("nested binding parses");
    handler
        .execute_parsed_commands_for_test(std::process::id(), binding)
        .await
        .expect("binding installs before it is triggered");

    let requester_pid = std::process::id();
    let (attach_tx, _attach_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), attach_tx)
        .await;
    let response = handler
        .handle(Request::SendKeysExt(rmux_proto::SendKeysExtRequest {
            target: Some(PaneTarget::with_window(alpha.clone(), 0, 0)),
            keys: vec![key],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: true,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await;
    let Response::Error(error) = response else {
        panic!("triggered binding must reject the unknown nested flag: {response:?}");
    };
    assert_eq!(
        error.error,
        RmuxError::Server(format!("command {command}: unknown flag -x")),
        "{invalid} did not fail closed when its binding was triggered"
    );
    assert_named_buffer_absent(&handler, &canary).await;
    assert_global_environment_absent(&handler, "-x").await;
    assert_stable_session(&handler, &alpha, pane_id).await;
}

#[test]
fn bind_key_nested_commands_reject_unknown_flags_without_effects() {
    run_on_daemon_test_stack(bind_key_nested_commands_reject_unknown_flags_body);
}

async fn bind_key_nested_commands_reject_unknown_flags_body() {
    for (index, (command, invalid)) in UNKNOWN_FLAG_COMMANDS.into_iter().enumerate() {
        Box::pin(assert_bind_key_case(index, command, invalid)).await;
    }
}

#[test]
fn parsed_queue_rejects_set_environment_unknown_flag_without_follow_on_effects() {
    run_on_daemon_test_stack(
        parsed_queue_rejects_set_environment_unknown_flag_without_follow_on_effects_body,
    );
}

async fn parsed_queue_rejects_set_environment_unknown_flag_without_follow_on_effects_body() {
    let handler = RequestHandler::new();
    let alpha = session_name("queue-parser-flags");
    let pane_id = create_stable_session(&handler, &alpha).await;
    let canary = "queue-set-environment-canary";
    let commands = CommandParser::new()
        .parse(&format!(
            "set-environment -g -x parser-flag-poison ; \
             set-buffer -b {canary} must-not-run"
        ))
        .expect("multi-command queue parses");

    let error = handler
        .execute_parsed_commands_for_test(std::process::id(), commands)
        .await
        .expect_err("set-environment unknown flag must abort its queue");

    assert_eq!(
        error,
        RmuxError::Server("command set-environment: unknown flag -x".to_owned())
    );
    assert_named_buffer_absent(&handler, canary).await;
    assert_global_environment_absent(&handler, "-x").await;
    assert_stable_session(&handler, &alpha, pane_id).await;
}

#[test]
fn parsed_queue_rejects_new_unknown_flags_without_follow_on_effects() {
    run_on_daemon_test_stack(parsed_queue_rejects_new_unknown_flags_without_follow_on_effects_body);
}

async fn parsed_queue_rejects_new_unknown_flags_without_follow_on_effects_body() {
    for (index, (command, invalid)) in UNKNOWN_FLAG_COMMANDS[NEW_UNKNOWN_FLAG_COMMAND_START..]
        .iter()
        .copied()
        .enumerate()
    {
        let handler = RequestHandler::new();
        let alpha = session_name("queue-new-parser-flags");
        let pane_id = create_stable_session(&handler, &alpha).await;
        let canary = format!("queue-new-parser-flag-{index}");
        let commands = CommandParser::new()
            .parse(&format!("{invalid} ; set-buffer -b {canary} must-not-run"))
            .unwrap_or_else(|error| panic!("multi-command queue parses for {invalid}: {error}"));

        let error = handler
            .execute_parsed_commands_for_test(std::process::id(), commands)
            .await
            .expect_err("unknown option must reject before its positional tail");

        assert_eq!(
            error,
            RmuxError::Server(format!("command {command}: unknown flag -x")),
            "{invalid} returned the wrong parser error"
        );
        assert_named_buffer_absent(&handler, &canary).await;
        assert_stable_session(&handler, &alpha, pane_id).await;
    }
}
