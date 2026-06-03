use super::*;

#[cfg(windows)]
const WINDOWS_ATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn attached_prefix_d_dispatches_detach_client() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02d")
        .await
        .expect("prefix d dispatches");

    // Entering and leaving the prefix key table now repaints the status bar
    // (so #{client_prefix} can show a prefix indicator), so the Detach control
    // may be preceded by status-refresh Write frames; scan past them.
    let mut detached = false;
    while let Ok(control) = control_rx.try_recv() {
        if matches!(control, AttachControl::Detach) {
            detached = true;
            break;
        }
    }
    assert!(detached, "C-b d must detach the attached client");
}

#[tokio::test]
async fn attached_prefix_d_dispatches_detach_client_across_separate_reads() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"d")
        .await
        .expect("prefix d input");

    let mut detached = false;
    while let Ok(control) = control_rx.try_recv() {
        if matches!(control, AttachControl::Detach) {
            detached = true;
            break;
        }
    }
    assert!(
        detached,
        "C-b d must still detach when prefix and command arrive in separate reads"
    );
}

#[tokio::test]
async fn attached_prefix_c_creates_window_across_separate_reads() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02")
        .await
        .expect("prefix key input");
    handler
        .handle_attached_live_input_for_test(requester_pid, b"c")
        .await
        .expect("prefix c input");

    assert_eq!(
        active_windows(&handler, &alpha).await,
        "0:0\n1:1\n",
        "C-b c must still create a new window when keys arrive in separate reads"
    );
}

#[tokio::test]
async fn attached_command_prompt_renames_current_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let state = handler.state.lock().await;
        if state.sessions.contains_session(&beta) {
            assert!(!state.sessions.contains_session(&alpha));
            break;
        }
        drop(state);
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for command prompt rename-session"
        );
        sleep(Duration::from_millis(25)).await;
    }

    let frame = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    assert!(
        !frame.contains("[alpha]"),
        "renamed session status must not keep old name: {frame:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn attached_exit_notifies_after_command_prompt_rename_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let _ = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    prepare_attached_shell_prompt(&handler, &PaneTarget::new(beta.clone(), 0)).await;
    drain_attach_controls(&mut control_rx);

    handler
        .handle_attached_live_input_for_test(requester_pid, b"exit\r")
        .await
        .expect("exit input after rename-session");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification after renamed exit");
    wait_for_session_removed(&handler, &beta).await;
}

#[cfg(windows)]
#[tokio::test]
async fn attached_windows_input_exits_after_command_prompt_rename_session() {
    // Windows consoles do not make byte 0x04 a reliable EOF signal, so this
    // uses a controlled line protocol to verify the post-rename attach target.
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx =
        create_line_exiting_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02:rename-session beta\r")
        .await
        .expect("prefix command prompt input");

    let _ = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    handler
        .handle_attached_live_input_for_test(requester_pid, b"RMUX_EXIT\r\n")
        .await
        .expect("Windows exit input after rename-session");

    tokio::time::timeout(WINDOWS_ATTACH_EXIT_TIMEOUT, async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification after renamed Windows input");
    wait_for_session_removed(&handler, &beta).await;
}

#[tokio::test]
async fn attached_session_status_updates_after_external_rename() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let renamed = handler
        .handle(Request::RenameSession(rmux_proto::RenameSessionRequest {
            target: alpha.clone(),
            new_name: beta.clone(),
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    let frame = wait_for_switch_frame_containing(&mut control_rx, "[beta]").await;
    assert!(
        !frame.contains("[alpha]"),
        "externally renamed session status must not keep old name: {frame:?}"
    );
}

#[tokio::test]
async fn attached_prefix_confirm_accepts_following_key_in_same_read_after_split() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let _control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02%")
        .await
        .expect("prefix split input");
    wait_for_active_panes(&handler, &alpha, "0:0\n1:1\n").await;

    handler
        .handle_attached_live_input_for_test(requester_pid, b"\x02xy")
        .await
        .expect("prefix confirm input");
    wait_for_active_panes(&handler, &alpha, "0:1\n").await;
}

#[tokio::test]
async fn attached_kill_last_pane_exits_the_session() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    let killed = handler
        .handle(Request::KillPane(rmux_proto::KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            window_destroyed: true,
        })
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match control_rx.recv().await {
                Some(AttachControl::Exited) => break,
                Some(_) => {}
                None => panic!("attach control channel closed before exit notification"),
            }
        }
    })
    .await
    .expect("timed out waiting for attach exit notification");
    wait_for_session_removed(&handler, &alpha).await;
}

async fn wait_for_active_panes(handler: &RequestHandler, session: &SessionName, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let panes = active_panes(handler, session).await;
        if panes == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for active panes {expected:?}, got {panes:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_switch_frame_containing(
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let Some(control) = tokio::time::timeout(Duration::from_millis(250), control_rx.recv())
            .await
            .expect("timed out waiting for attach refresh")
        else {
            panic!("attach refresh channel closed");
        };
        if let AttachControl::Switch(target) = control {
            let frame = String::from_utf8(target.render_frame).expect("render frame is utf-8");
            if frame.contains(expected) {
                return frame;
            }
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for attach frame containing {expected:?}"
        );
    }
}

#[tokio::test]
async fn attached_resize_resizes_session_and_refreshes_status_frame() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_resize(
            requester_pid,
            TerminalSize {
                cols: 132,
                rows: 43,
            },
        )
        .await
        .expect("attached resize succeeds");

    {
        let client_size = {
            let active_attach = handler.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&requester_pid)
                .expect("attached client is tracked")
                .client_size
        };
        let state = handler.state.lock().await;
        let size = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .size();
        assert_eq!(
            client_size,
            TerminalSize {
                cols: 132,
                rows: 43
            }
        );
        assert_eq!(
            size,
            TerminalSize {
                cols: 132,
                rows: 43
            }
        );
    }
    assert_eq!(
        pane_terminal_size(&handler, &alpha, 0, 0).await,
        TerminalSize {
            cols: 132,
            rows: 42
        }
    );
    let frame = take_render_frame(control_rx.try_recv().expect("resize refresh"));
    assert!(
        frame.contains("[alpha]"),
        "resize should redraw status for the attached client, got {frame:?}"
    );
}

#[tokio::test]
async fn attached_refresh_renders_each_client_at_its_own_size() {
    let handler = RequestHandler::new();
    let local_pid = 101;
    let browser_pid = 202;
    let alpha = session_name("alpha");
    let mut local_rx = create_attached_session(&handler, local_pid, &alpha).await;
    let (browser_tx, mut browser_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(browser_pid, alpha.clone(), browser_tx)
        .await;

    handler
        .handle_attached_resize(
            browser_pid,
            TerminalSize {
                cols: 132,
                rows: 43,
            },
        )
        .await
        .expect("browser resize succeeds");

    let local_frame = take_render_frame(local_rx.try_recv().expect("local refresh"));
    let browser_frame = take_render_frame(browser_rx.try_recv().expect("browser refresh"));
    assert!(
        local_frame.contains("\x1b[24;1H"),
        "local attach must keep a 24-row status line, got {local_frame:?}"
    );
    assert!(
        !local_frame.contains("\x1b[43;1H"),
        "local attach must not receive browser-sized redraws, got {local_frame:?}"
    );
    assert!(
        browser_frame.contains("\x1b[43;1H"),
        "browser attach should render at the browser-requested height, got {browser_frame:?}"
    );

    handler
        .refresh_attached_client_status(local_pid, &alpha)
        .await
        .expect("status refresh succeeds");
    let local_status = match local_rx.try_recv().expect("local status refresh") {
        AttachControl::Write(bytes) => String::from_utf8(bytes).expect("status is utf-8"),
        other => panic!("expected status write, got {other:?}"),
    };
    assert!(
        local_status.contains("\x1b[24;1H"),
        "periodic status refresh must keep the local client height, got {local_status:?}"
    );
    assert!(
        !local_status.contains("\x1b[43;1H"),
        "periodic status refresh must not use the browser height, got {local_status:?}"
    );
}

#[tokio::test]
async fn attached_resize_ignores_zero_sized_terminal_reports() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let mut control_rx = create_attached_session(&handler, requester_pid, &alpha).await;

    handler
        .handle_attached_resize(requester_pid, TerminalSize { cols: 0, rows: 0 })
        .await
        .expect("zero-sized resize is ignored");

    let (client_size, session_size) = {
        let active_attach = handler.active_attach.lock().await;
        let client_size = active_attach
            .by_pid
            .get(&requester_pid)
            .expect("attached client is tracked")
            .client_size;
        drop(active_attach);

        let state = handler.state.lock().await;
        let session_size = state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .size();
        (client_size, session_size)
    };

    assert_eq!(client_size, TerminalSize { cols: 80, rows: 24 });
    assert_eq!(session_size, TerminalSize { cols: 80, rows: 24 });
    assert!(
        control_rx.try_recv().is_err(),
        "ignored zero-sized resize must not emit a refresh frame"
    );
}
