use super::*;
use crate::input_keys::MouseForwardEvent;
use crate::mouse::{AttachedMouseEvent, MouseLocation};
use rmux_core::{input::InputParser, PaneId, Screen};
use tokio::sync::mpsc;

#[cfg(unix)]
fn quiet_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(unix)]
fn failing_pipe_command() -> &'static str {
    "exit 7"
}

#[cfg(windows)]
fn failing_pipe_command() -> &'static str {
    "exit /b 7"
}

#[cfg(windows)]
fn quiet_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = std::path::PathBuf::from(system_root)
        .join("System32")
        .join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn fixture(name: &str) -> (RequestHandler, SessionName, PaneTarget) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 6 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::CopyModeLineNumbers,
                value: "absolute".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    (handler, session, target)
}

fn mouse_event(target: &PaneTarget) -> AttachedMouseEvent {
    AttachedMouseEvent {
        raw: MouseForwardEvent {
            b: 0,
            lb: 0,
            x: 1,
            y: 1,
            lx: 1,
            ly: 1,
            sgr_b: 0,
            sgr_type: 'M',
            ignore: false,
        },
        session_id: 1,
        window_id: Some(1),
        pane_id: Some(PaneId::new(0)),
        pane_target: Some(target.clone()),
        location: MouseLocation::Pane,
        status_at: None,
        status_lines: 0,
        ignore: false,
    }
}

async fn execute_with_mouse_event(
    handler: &RequestHandler,
    target: &PaneTarget,
    command: &str,
    event: AttachedMouseEvent,
) {
    execute_with_mouse_event_result(handler, target, command, event)
        .await
        .unwrap_or_else(|error| panic!("command {command:?} executes: {error}"));
}

async fn execute_with_mouse_event_result(
    handler: &RequestHandler,
    target: &PaneTarget,
    command: &str,
    event: AttachedMouseEvent,
) -> Result<rmux_proto::CommandOutput, rmux_proto::RmuxError> {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("command {command:?} parses: {error}"));
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(target.clone())))
                .with_mouse_target(Some(Target::Pane(target.clone())))
                .with_mouse_event(Some(event)),
        )
        .await
}

async fn execute_with_mouse(handler: &RequestHandler, target: &PaneTarget, command: &str) {
    execute_with_mouse_event(handler, target, command, mouse_event(target)).await;
}

async fn execute_with_mouse_target(
    handler: &RequestHandler,
    current_target: PaneTarget,
    mouse_target: PaneTarget,
    command: &str,
) -> rmux_proto::CommandOutput {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("command {command:?} parses: {error}"));
    handler
        .execute_parsed_commands(
            std::process::id(),
            parsed,
            QueueExecutionContext::without_caller_cwd()
                .with_current_target(Some(Target::Pane(current_target)))
                .with_mouse_target(Some(Target::Pane(mouse_target))),
        )
        .await
        .unwrap_or_else(|error| panic!("command {command:?} executes: {error}"))
}

async fn execute_without_mouse(handler: &RequestHandler, command: &str) {
    let parsed = CommandParser::new()
        .parse(command)
        .unwrap_or_else(|error| panic!("command {command:?} parses: {error}"));
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap_or_else(|error| panic!("command {command:?} executes: {error}"));
}

async fn prepare_copy_mode_fixture(
    handler: &RequestHandler,
    session: &SessionName,
    target: &PaneTarget,
) -> mpsc::UnboundedReceiver<crate::pane_io::AttachControl> {
    let transcript = {
        let state = handler.state.lock().await;
        state.transcript_handle(target).expect("pane transcript")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex")
        .history_limit();
    let mut screen = Screen::new(TerminalSize { cols: 20, rows: 6 }, history_limit);
    let mut parser = InputParser::new();
    parser.parse(
        b"zero one two three\r\nalpha beta gamma\r\nomega sigma tau\r\n",
        &mut screen,
    );
    transcript
        .lock()
        .expect("pane transcript mutex")
        .set_screen_for_test(screen);

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(std::process::id(), session.clone(), control_tx)
        .await;
    execute_without_mouse(
        handler,
        &format!(
            "copy-mode -t {target}; send-keys -Xt {target} history-top; \
             send-keys -Xt {target} start-of-line; send-keys -N6 -Xt {target} cursor-right"
        ),
    )
    .await;
    control_rx
}

async fn selection_coordinates(
    handler: &RequestHandler,
    session: &SessionName,
) -> Option<(u32, usize)> {
    let state = handler.state.lock().await;
    state
        .pane_copy_mode_summary(session, PaneId::new(0))
        .and_then(|summary| summary.selection_start)
        .map(|position| (position.x, position.y))
}

async fn cursor_coordinates(
    handler: &RequestHandler,
    session: &SessionName,
) -> Option<(u32, usize)> {
    let state = handler.state.lock().await;
    state
        .pane_copy_mode_summary(session, PaneId::new(0))
        .map(|summary| (summary.cursor_x, summary.cursor_y))
}

async fn unnamed_buffer(handler: &RequestHandler) -> Vec<u8> {
    handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await
        .command_output()
        .expect("show-buffer returns command output")
        .stdout()
        .to_vec()
}

async fn wait_for_line_number_state(
    handler: &RequestHandler,
    session: &SessionName,
    pane_id: PaneId,
    expected: bool,
) {
    tokio::time::timeout(background_shell_test_timeout(), async {
        loop {
            let actual = {
                let state = handler.state.lock().await;
                state
                    .pane_copy_mode_summary(session, pane_id)
                    .map(|summary| summary.line_numbers_enabled)
            };
            if actual == Some(expected) {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("copy-mode line_numbers_enabled did not become {expected} for {session}")
    });
}

async fn wait_for_display_panes_state(handler: &RequestHandler) {
    let requester_pid = std::process::id();
    tokio::time::timeout(background_shell_test_timeout(), async {
        loop {
            let active = {
                let active_attach = handler.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&requester_pid)
                    .is_some_and(|active| active.display_panes.is_some())
            };
            if active {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("display-panes state should become active");
}

async fn assert_mouse_target_queue_case(
    name: &str,
    command: &str,
    sourced: bool,
    background: bool,
    expected_pane: u32,
) -> rmux_proto::CommandOutput {
    let (handler, session, current_target) = fixture(name).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(current_target.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let mouse_target = PaneTarget::with_window(session.clone(), 0, 1);
    handler
        .wait_for_pane_startup_to_finish_for_test(&mouse_target)
        .await;
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: current_target.clone(),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");

    let root = sourced.then(|| temp_root(name));
    let queued_command = match root.as_ref() {
        Some(root) => {
            let path = root.join("mouse-target.conf");
            write_config(&path, &format!("{command}\n"));
            format!("source-file {}", shell_quote(&path))
        }
        None => command.to_owned(),
    };
    let output =
        execute_with_mouse_target(&handler, current_target, mouse_target, &queued_command).await;
    if background {
        wait_for_detached_request_count(&handler, 0).await;
    }

    let active_pane = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .expect("session exists")
            .window_at(0)
            .expect("window exists")
            .active_pane_index()
    };
    assert_eq!(active_pane, expected_pane, "{queued_command}");
    if let Some(root) = root {
        let _ = std::fs::remove_dir_all(root);
    }
    output
}

#[tokio::test]
async fn mouse_origin_survives_direct_source_and_foreground_command_queues() {
    // Oracle tmux 3.7b: the originating mouse event survives direct queue
    // dispatch, source-file, foreground if-shell, and foreground run-shell -C.
    for (name, command) in [
        ("mouse-direct-copy", "copy-mode"),
        ("mouse-if-shell-copy", "if-shell -F 1 { copy-mode }"),
        ("mouse-run-shell-copy", "run-shell -C 'copy-mode'"),
    ] {
        let (handler, session, target) = fixture(name).await;
        execute_with_mouse(&handler, &target, command).await;
        wait_for_line_number_state(&handler, &session, PaneId::new(0), false).await;
    }

    let (handler, session, target) = fixture("mouse-source-copy").await;
    let root = temp_root("mouse-origin-copy-mode");
    let path = root.join("copy.conf");
    write_config(&path, "copy-mode\n");
    execute_with_mouse(
        &handler,
        &target,
        &format!("source-file {}", shell_quote(&path)),
    )
    .await;
    wait_for_line_number_state(&handler, &session, PaneId::new(0), false).await;
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn background_command_queues_drop_mouse_origin_like_tmux() {
    // Oracle tmux 3.7b: detached jobs start with a fresh cmdq state, so
    // absolute line numbers remain enabled in the commands they later run.
    for (name, command) in [
        (
            "mouse-background-if-copy",
            format!(
                "if-shell -b {} {{ copy-mode }}",
                command_quote(&delayed_true_shell_condition())
            ),
        ),
        (
            "mouse-background-run-copy",
            "run-shell -bC 'copy-mode'".to_owned(),
        ),
    ] {
        let (handler, session, target) = fixture(name).await;
        use_platform_test_shell(&handler).await;
        execute_with_mouse(&handler, &target, &command).await;
        wait_for_line_number_state(&handler, &session, PaneId::new(0), true).await;
        wait_for_detached_request_count(&handler, 0).await;
    }
}

#[tokio::test]
async fn mouse_origin_reaches_copy_commands_through_foreground_queues() {
    // Oracle tmux 3.7b: the originating event survives direct dispatch,
    // foreground if-shell/run-shell -C, and source-file.
    for (name, command) in [
        ("mouse-direct-selection", "send-keys -X begin-selection"),
        (
            "mouse-if-selection",
            "if-shell -F 1 { send-keys -X begin-selection }",
        ),
        (
            "mouse-run-selection",
            "run-shell -C 'send-keys -X begin-selection'",
        ),
    ] {
        let (handler, session, target) = fixture(name).await;
        let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
        execute_with_mouse(&handler, &target, command).await;
        assert_eq!(
            selection_coordinates(&handler, &session).await,
            Some((0, 1)),
            "{command} must use its originating mouse event"
        );
    }

    let (handler, session, target) = fixture("mouse-source-selection").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
    let root = temp_root("mouse-origin-copy-selection");
    let path = root.join("selection.conf");
    write_config(&path, "send-keys -X begin-selection\n");
    execute_with_mouse(
        &handler,
        &target,
        &format!("source-file {}", shell_quote(&path)),
    )
    .await;
    assert_eq!(
        selection_coordinates(&handler, &session).await,
        Some((0, 1)),
        "source-file must preserve its foreground mouse origin"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn background_copy_commands_ignore_the_cached_mouse_event() {
    // Oracle tmux 3.7b: detached jobs start a fresh command queue. Even though
    // the attached client still caches the event, the later command is not a
    // mouse command and must preserve the keyboard cursor.
    for (name, command) in [
        (
            "mouse-background-if-selection",
            format!(
                "if-shell -b {} {{ send-keys -X begin-selection }}",
                command_quote(&delayed_true_shell_condition())
            ),
        ),
        (
            "mouse-background-run-selection",
            "run-shell -bC 'send-keys -X begin-selection'".to_owned(),
        ),
    ] {
        let (handler, session, target) = fixture(name).await;
        use_platform_test_shell(&handler).await;
        let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
        {
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&std::process::id())
                .expect("attached client exists");
            active.mouse.current_event = Some(mouse_event(&target));
        }
        execute_with_mouse(&handler, &target, &command).await;
        wait_for_detached_request_count(&handler, 0).await;
        assert_eq!(
            selection_coordinates(&handler, &session).await,
            Some((6, 0)),
            "{command} must not revive the cached mouse event"
        );
    }
}

#[tokio::test]
async fn foreground_command_queues_preserve_mouse_targets() {
    for (selector_name, selector) in [("equal", "="), ("long", "'{mouse}'")] {
        let action = format!("select-pane -t {selector}");
        for (queue_name, command, sourced) in [
            ("direct", action.clone(), false),
            ("if", format!("if-shell -F 1 {{ {action} }}"), false),
            (
                "run",
                format!("run-shell -C {}", command_quote(&action)),
                false,
            ),
            ("source", action.clone(), true),
        ] {
            let _ = assert_mouse_target_queue_case(
                &format!("mouse-target-foreground-{selector_name}-{queue_name}"),
                &command,
                sourced,
                false,
                1,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn background_command_queues_drop_mouse_targets_like_tmux() {
    for (selector_name, selector) in [("equal", "="), ("long", "'{mouse}'")] {
        let action = format!("select-pane -t {selector}");
        let if_shell = format!(
            "if-shell -b {} {{ {action} }}",
            command_quote(&delayed_true_shell_condition())
        );
        let run_shell = format!("run-shell -bC {}", command_quote(&action));
        for (queue_name, command, sourced) in [
            ("if", if_shell.clone(), false),
            ("run", run_shell.clone(), false),
            ("source-if", if_shell, true),
            ("source-run", run_shell, true),
        ] {
            let _ = assert_mouse_target_queue_case(
                &format!("mouse-target-background-{selector_name}-{queue_name}"),
                &command,
                sourced,
                true,
                0,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn format_if_shell_ignores_background_flag_and_preserves_mouse_target() {
    // tmux 3.7b executes -F branches inline even when -b is also present.
    for sourced in [false, true] {
        let output = assert_mouse_target_queue_case(
            &format!("mouse-target-format-background-flag-{sourced}"),
            "if-shell -bF 1 { select-pane -t = ; display-message -p marker }",
            sourced,
            false,
            1,
        )
        .await;
        assert_eq!(output.stdout(), b"marker\n");
    }
}

#[tokio::test]
async fn display_menu_action_preserves_its_mouse_origin() {
    // Oracle tmux 3.7b: a menu action inherits the event from the
    // display-menu queue item even when the choice is accepted by keyboard.
    let (handler, session, target) = fixture("mouse-display-menu-action").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;

    execute_with_mouse(
        &handler,
        &target,
        r#"display-menu -T Menu "Select" "x" { send-keys -X begin-selection }"#,
    )
    .await;
    handler
        .handle_attached_live_input_for_test(std::process::id(), b"x")
        .await
        .expect("menu shortcut input");

    assert_eq!(
        selection_coordinates(&handler, &session).await,
        Some((0, 1)),
        "display-menu action must use the click that opened the menu"
    );
}

#[tokio::test]
async fn foreground_display_panes_action_preserves_its_mouse_origin() {
    // Oracle tmux 3.7b: foreground display-panes retains its queue item and
    // inserts the selected template with that item's mouse event.
    let (handler, session, target) = fixture("mouse-display-panes-action").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
    let execution_handler = handler.clone();
    let execution_target = target.clone();
    let execution = tokio::spawn(async move {
        execute_with_mouse(
            &execution_handler,
            &execution_target,
            "display-panes -d 60000 'send-keys -X begin-selection'",
        )
        .await;
    });

    wait_for_display_panes_state(&handler).await;
    handler
        .handle_attached_live_input_for_test(std::process::id(), b"0")
        .await
        .expect("display-panes label input");
    assert_eq!(
        selection_coordinates(&handler, &session).await,
        Some((0, 1)),
        "foreground display-panes action must use the click that opened it"
    );

    execution.abort();
    let _ = execution.await;
}

#[tokio::test]
async fn display_panes_rekeys_mouse_origin_outside_the_target_client_session() {
    let (handler, alpha, alpha_target) = fixture("mouse-display-panes-origin-alpha").await;
    let beta = session_name("mouse-display-panes-client-beta");
    let beta_target = PaneTarget::with_window(beta.clone(), 0, 0);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(beta.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 20, rows: 6 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&beta_target)
        .await;

    let _alpha_control_rx = prepare_copy_mode_fixture(&handler, &alpha, &alpha_target).await;
    let requester_pid = std::process::id();
    let (beta_control_tx, _beta_control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(requester_pid, beta.clone(), beta_control_tx)
        .await;

    let execution_handler = handler.clone();
    let execution_target = alpha_target.clone();
    let execution = tokio::spawn(async move {
        execute_with_mouse(
            &execution_handler,
            &execution_target,
            &format!(
                "display-panes -d 60000 -t {requester_pid} \
                 'send-keys -X -t = begin-selection'"
            ),
        )
        .await;
    });
    wait_for_display_panes_state(&handler).await;

    let renamed_alpha = session_name("mouse-display-panes-origin-renamed");
    let response = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha,
            new_name: renamed_alpha.clone(),
        }))
        .await;
    assert!(
        matches!(response, Response::RenameSession(_)),
        "{response:?}"
    );
    {
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        assert_eq!(active.session_name, beta);
        assert_eq!(
            active
                .display_panes
                .as_ref()
                .expect("display-panes remains active")
                .window
                .session_name(),
            &beta,
            "renaming the origin must not rekey the target client's labels"
        );
    }

    handler
        .handle_attached_live_input_for_test(requester_pid, b"0")
        .await
        .expect("display-panes label resolves the renamed mouse target");
    assert_eq!(
        selection_coordinates(&handler, &renamed_alpha).await,
        Some((0, 1)),
        "the deferred event and mouse target must follow their origin session rename"
    );

    execution.abort();
    let _ = execution.await;
}

#[tokio::test]
async fn background_display_panes_action_drops_its_mouse_origin() {
    // Oracle tmux 3.7b: display-panes -b has no retained queue item, so its
    // selected template starts from a fresh state without the opening click.
    let (handler, session, target) = fixture("mouse-background-display-panes-action").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;

    execute_with_mouse(
        &handler,
        &target,
        "display-panes -b -d 60000 'send-keys -X begin-selection'",
    )
    .await;
    wait_for_display_panes_state(&handler).await;
    handler
        .handle_attached_live_input_for_test(std::process::id(), b"0")
        .await
        .expect("background display-panes label input");

    assert_eq!(
        selection_coordinates(&handler, &session).await,
        Some((6, 0)),
        "display-panes -b action must preserve the keyboard cursor"
    );
}

#[tokio::test]
async fn mouse_origin_repositions_before_generic_copy_mode_commands() {
    // Oracle tmux 3.7b: every valid non-wheel mouse event moves the copy-mode
    // cursor before dispatching the requested command.
    for (name, command, expected) in [
        (
            "mouse-direct-cursor-right",
            "send-keys -X cursor-right",
            (1, 1),
        ),
        (
            "mouse-repeated-cursor-right",
            "send-keys -N3 -X cursor-right",
            (3, 1),
        ),
        (
            "mouse-if-cursor-right",
            "if-shell -F 1 { send-keys -X cursor-right }",
            (1, 1),
        ),
        (
            "mouse-run-cursor-right",
            "run-shell -C 'send-keys -X cursor-right'",
            (1, 1),
        ),
    ] {
        let (handler, session, target) = fixture(name).await;
        let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;

        execute_with_mouse(&handler, &target, command).await;

        assert_eq!(
            cursor_coordinates(&handler, &session).await,
            Some(expected),
            "{command} must move to the mouse position before cursor-right"
        );
    }
}

#[tokio::test]
async fn mouse_origin_repositions_before_copy_line_transfer() {
    let (handler, session, target) = fixture("mouse-copy-line").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;

    execute_with_mouse(&handler, &target, "send-keys -X copy-line").await;

    assert_eq!(cursor_coordinates(&handler, &session).await, Some((0, 1)));
    assert_eq!(unnamed_buffer(&handler).await, b"alpha beta gamma");
}

#[tokio::test]
async fn counted_line_transfer_family_runs_once_through_the_shared_queue() {
    // Oracle tmux 3.7b: all eight commands consume -N3 as one transfer.
    // Pipe variants start one job, and only and-cancel variants leave copy mode.
    for (command, pipe, cancel, end_of_line) in [
        ("copy-line", false, false, false),
        ("copy-line-and-cancel", false, true, false),
        ("copy-pipe-line", true, false, false),
        ("copy-pipe-line-and-cancel", true, true, false),
        ("copy-end-of-line", false, false, true),
        ("copy-end-of-line-and-cancel", false, true, true),
        ("copy-pipe-end-of-line", true, false, true),
        ("copy-pipe-end-of-line-and-cancel", true, true, true),
    ] {
        let (handler, session, target) = fixture(&format!("prefix-{command}")).await;
        let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
        let position = if end_of_line {
            "send-keys -X history-top; send-keys -X start-of-line; \
             send-keys -N2 -X cursor-right; "
        } else {
            "send-keys -X history-top; send-keys -X start-of-line; "
        };
        let pipe_command = if pipe {
            format!(" -- {}", command_quote(failing_pipe_command()))
        } else {
            String::new()
        };

        execute_without_mouse(
            &handler,
            &format!("{position}send-keys -N3 -X {command}{pipe_command}"),
        )
        .await;

        let expected = if end_of_line {
            b"ro one two three\nalpha beta gamma\nomega sigma tau".as_slice()
        } else {
            b"zero one two three\nalpha beta gamma\nomega sigma tau".as_slice()
        };
        assert_eq!(unnamed_buffer(&handler).await, expected, "{command}");
        let (buffer_count, mode_active) = {
            let state = handler.state.lock().await;
            (
                state.buffers.len(),
                state
                    .pane_copy_mode_summary(&session, PaneId::new(0))
                    .is_some(),
            )
        };
        assert_eq!(buffer_count, 1, "{command} must create one buffer");
        assert_eq!(mode_active, !cancel, "{command}");
    }
}

#[tokio::test]
async fn nonzero_copy_pipe_exit_is_startup_success_and_finalizes_mouse_refresh() {
    let (handler, session, target) = fixture("mouse-transfer-nonzero-exit").await;
    let mut control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
    while control_rx.try_recv().is_ok() {}
    let mut lifecycle = handler.subscribe_lifecycle_events();

    let result = execute_with_mouse_event_result(
        &handler,
        &target,
        &format!(
            "send-keys -X copy-pipe-and-cancel -- {}",
            command_quote(failing_pipe_command())
        ),
        mouse_event(&target),
    )
    .await;

    assert!(
        result.is_ok(),
        "a successfully started pipe is a synchronous success even when its child exits nonzero"
    );
    assert_eq!(
        cursor_coordinates(&handler, &session).await,
        None,
        "copy-pipe-and-cancel must leave copy mode after the child starts"
    );
    assert!(
        std::iter::from_fn(|| lifecycle.try_recv().ok()).any(|queued| matches!(
            queued.event,
            rmux_core::LifecycleEvent::PaneModeChanged { target: ref changed }
                if changed == &target
        )),
        "the cancel transition must emit pane-mode-changed after the pipe starts"
    );
    let refresh_count = std::iter::from_fn(|| control_rx.try_recv().ok())
        .filter(|control| {
            matches!(
                control,
                crate::pane_io::AttachControl::Refresh | crate::pane_io::AttachControl::Switch(_)
            )
        })
        .count();
    assert!(
        refresh_count >= 1,
        "the coalesced session refresh must cover the final state of both panes"
    );
}

#[tokio::test]
async fn wheel_origin_does_not_reposition_generic_copy_mode_commands() {
    let (handler, session, target) = fixture("wheel-cursor-right").await;
    let _control_rx = prepare_copy_mode_fixture(&handler, &session, &target).await;
    let mut wheel = mouse_event(&target);
    wheel.raw.b = 64;
    wheel.raw.sgr_b = 64;

    execute_with_mouse_event(&handler, &target, "send-keys -X cursor-right", wheel).await;

    assert_eq!(cursor_coordinates(&handler, &session).await, Some((7, 0)));
}

#[tokio::test]
async fn command_hook_preserves_mouse_origin_like_tmux() {
    let (handler, session, target) = fixture("mouse-hook-copy").await;
    execute_without_mouse(&handler, "split-window -h -t mouse-hook-copy:0.0").await;
    execute_without_mouse(&handler, "select-pane -t mouse-hook-copy:0.0").await;
    execute_without_mouse(&handler, "set-hook -g after-select-pane { copy-mode }").await;

    execute_with_mouse(&handler, &target, "select-pane -t mouse-hook-copy:0.1").await;
    let active_pane = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .expect("session exists")
            .window()
            .active_pane()
            .expect("active pane exists")
            .id()
    };
    wait_for_line_number_state(&handler, &session, active_pane, false).await;
}
