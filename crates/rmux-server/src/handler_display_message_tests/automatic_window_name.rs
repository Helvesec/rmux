//! One automatic window name per expansion.
//!
//! tmux answers `#W`, the `S:`/`W:` loop bodies and `N:` from the window's one
//! stored `w->name`, derived from its active pane (`names.c`). Measured on
//! tmux 3.7b (macOS, `SHELL=/bin/sh`, `-f /dev/null`): `new -d -s a`, then
//! `splitw -t a:0 'sleep 100'`, then
//! `display -p -t a:0.0` with the template asserted below printed
//! `0|sleep|sleep|[sleep]|0[1]|1` in 20/20 runs, although the targeted pane 0
//! runs the shell.

use super::*;
#[cfg(unix)]
use rmux_proto::{ProcessCommand, SplitWindowExtRequest};

async fn display(handler: &RequestHandler, target: PaneTarget, message: &str) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some(message.to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response for {message}");
    };
    String::from_utf8(
        response
            .command_output()
            .expect("display-message -p returns output")
            .stdout()
            .to_vec(),
    )
    .expect("display-message output is utf-8")
}

#[cfg(unix)]
#[tokio::test]
async fn display_message_names_window_once_from_its_active_pane() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    let split = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
            command: None,
            // Exec sleep directly: through a /bin/sh trampoline the pane would
            // report the runtime name before sleep is actually in the foreground.
            process_command: Some(ProcessCommand::Argv(vec![
                "/bin/sleep".to_owned(),
                "30".to_owned(),
            ])),
            start_directory: None,
            keep_alive_on_exit: None,
            detached: false,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: None,
        })))
        .await;
    let Response::SplitWindow(split) = split else {
        panic!("expected split-window response");
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&split.pane)
        .await;

    // The active pane 1 runs sleep; the targeted pane 0 runs the shell.
    let active = PaneTarget::with_window(alpha.clone(), 0, 1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let probe = display(
            &handler,
            active.clone(),
            "#{pane_active}:#{pane_current_command}",
        )
        .await;
        if probe == "1:sleep\n" {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "active pane never reported sleep, last {probe:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let output = display(
        &handler,
        PaneTarget::with_window(alpha, 0, 0),
        "#{pane_active}|#W|#{S:#W}|#{W:#W,[#W]}|#{P:#{pane_index},[#{pane_index}]}|#{N:#W}",
    )
    .await;
    assert_eq!(output, "0|sleep|sleep|[sleep]|0[1]|1\n");
}

/// The automatic name stays live (no rename-window pin): whatever process the
/// active pane runs, one expansion names the window once, so the session loop,
/// the current-window body and `N:` all agree. Under `SHELL=/bin/sh` the live
/// name moves from `sh` to `bash` while the trampoline execs; tmux 3.7b
/// printed `sh|[sh]|0[1]|1` or `bash|[bash]|0[1]|1` in 100/100 expansions.
#[tokio::test]
async fn display_message_print_expands_runtime_session_window_and_pane_loops() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(PaneTarget::with_window(alpha.clone(), 0, 0)),
                direction: SplitDirection::Horizontal,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));

    let output = display(
        &handler,
        PaneTarget::with_window(alpha, 0, 0),
        "#{S:#W}|#{W:#W,[#W]}|#{P:#{pane_index},[#{pane_index}]}|#{N:#W}",
    )
    .await;
    let name = output.split('|').next().unwrap_or_default();
    assert!(
        !name.is_empty(),
        "the session loop printed no window name: {output:?}"
    );
    assert_eq!(output, format!("{name}|[{name}]|0[1]|1\n"));
}

/// A new window stores its shell's name from creation (tmux `spawn.c` sets
/// `w->name = default_window_name(w)`, the shell's basename). tmux 3.7b on
/// Linux (`SHELL=/bin/sh`, `-f /dev/null`, `new -d` then `lsw -F
/// '#{window_name}'` at once) printed `sh` in 6/10 runs; the other 4 printed
/// `tmux`, from its first rename racing the child's exec. RMUX used to store
/// its program name `rmux` until that rename, ~58 ms later.
#[cfg(unix)]
#[tokio::test]
async fn a_new_window_stores_its_shell_name_before_any_automatic_rename() {
    let handler = RequestHandler::with_host_shell_for_test();
    let alpha = session_name("alpha");
    handler
        .state
        .lock()
        .await
        .options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("test default-shell is valid");

    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: None,
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    for window_index in [0, 1] {
        let window = session.window_at(window_index).expect("window exists");
        assert_eq!(
            window.name(),
            Some("sh"),
            "stored name of {alpha}:{window_index}"
        );
    }
}
