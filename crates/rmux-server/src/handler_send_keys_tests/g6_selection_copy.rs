//! The G6 selection/copy parcours, driven from a real client's own mouse reports.
//!
//! Every expectation here was measured on tmux 3.7b before this module was written: a private
//! server per case, a real client attached on a pty of the same size, the same three reports
//! written to that pty, and the result read back with `show-buffer` and `#{pane_mode}`. The
//! measurements and the method are in `g6-selection-copy-r1/ORACLE-G6.json` and
//! `ORACLE-G6-APPLICATION.json`. RMUX's own output is never the oracle, so the expected bytes are
//! written out here.
//!
//! On a 20x5 pane holding three known lines, a press at column 5 row 2 and a drag and release at
//! column 10 row 3 give two different results, decided by whether the pane application asked for
//! a mouse mode:
//!
//! * no application mouse mode: the root binding `MouseDrag1Pane` takes its `copy-mode -M` branch,
//!   the pane enters copy mode with the selection anchored at the **press** cell, the release runs
//!   `copy-pipe-and-cancel` from the copy-mode table, copy mode ends, and the buffer holds
//!   `a beta gamma\nomega sig` - 22 bytes. The application is handed nothing.
//! * application in 1002 + 1006: nothing is copied at all, tmux answering `no buffers`, the pane
//!   never enters copy mode, and the application is handed the three reports byte for byte.
//!
//! This is the whole chain from the client bytes through the bindings, not a helper called by hand.

use super::*;
use rmux_core::{input::InputParser, Screen};

const SIZE: TerminalSize = TerminalSize { cols: 20, rows: 5 };
const CONTENT: &[u8] = b"zero one two three\r\nalpha beta gamma\r\nomega sigma tau\r\n";
/// What the terminal sends to RMUX: press, drag, release of button 1.
const PRESS: &[u8] = b"\x1b[<0;5;2M";
const DRAG: &[u8] = b"\x1b[<32;10;3M";
const RELEASE: &[u8] = b"\x1b[<0;10;3m";
/// A second drag further right, and its release there.
const DRAG_FAR: &[u8] = b"\x1b[<32;15;3M";
const RELEASE_FAR: &[u8] = b"\x1b[<0;15;3m";
/// A release back at the press cell: a bare click, with no drag at all.
const RELEASE_AT_PRESS: &[u8] = b"\x1b[<0;5;2m";
/// Measured on tmux 3.7b for exactly those reports on exactly that content.
const COPIED: &[u8] = b"a beta gamma\nomega sig";
/// Measured the same way when a second drag extends the selection before the release.
const COPIED_EXTENDED: &[u8] = b"a beta gamma\nomega sigma ta";
/// The three reports as the application receives them, measured the same way.
const FORWARDED: &[u8] = b"\x1b[<0;5;2M\x1b[<32;10;3M\x1b[<0;10;3m";
/// The application modes of the negative case, asked for by the application itself.
const APPLICATION_MODES: &[u8] = b"\x1b[?1002h\x1b[?1006h";

#[cfg(unix)]
fn quiet_pane_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_pane_command() -> Vec<String> {
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

/// A session whose pane holds the measured content, with one attached client and the mouse on.
/// The control receiver is handed back because the attach only lives as long as it does.
async fn fixture(
    name: &str,
) -> (
    RequestHandler,
    rmux_proto::SessionName,
    PaneTarget,
    PaneId,
    u32,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let target = PaneTarget::new(session.clone(), 0);
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(SIZE),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_pane_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);

    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(&target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript.lock().history_limit();
    let mut screen = Screen::new(SIZE, history_limit);
    let mut parser = InputParser::new();
    parser.parse(CONTENT, &mut screen);
    transcript.lock().set_screen_for_test(screen);

    let mouse = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(mouse, Response::SetOption(_)), "{mouse:?}");

    let requester_pid = std::process::id();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;

    let pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&session)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(rmux_core::Pane::id)
            .expect("pane exists")
    };
    (handler, session, target, pane_id, requester_pid, control_rx)
}

async fn send(handler: &RequestHandler, pid: u32, reports: &[&[u8]]) {
    for report in reports {
        handler
            .handle_attached_live_input_for_test(pid, report)
            .await
            .expect("live attach mouse input");
    }
}

async fn copy_mode(
    handler: &RequestHandler,
    session: &rmux_proto::SessionName,
    pane_id: PaneId,
) -> Option<crate::copy_mode::CopyModeSummary> {
    let state = handler.state.lock().await;
    state.pane_copy_mode_summary(session, pane_id)
}

/// What RMUX wrote toward the pane application since the capture started.
async fn handed_to_the_application(handler: &RequestHandler, target: &PaneTarget) -> Vec<u8> {
    let state = handler.state.lock().await;
    state
        .pane_input_capture_for_test(target)
        .expect("the pane input capture is running")
}

/// tmux 3.7b, no application mouse mode: press, drag and release copy
/// `a beta gamma\nomega sig` and hand the application nothing.
#[tokio::test]
async fn a_mouse_drag_copies_exactly_what_tmux_copies() {
    let (handler, session, target, pane_id, pid, _control_rx) = fixture("g6-selection").await;
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
    }

    send(&handler, pid, &[PRESS]).await;
    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "the press alone must not enter copy mode"
    );

    send(&handler, pid, &[DRAG]).await;
    let dragged = copy_mode(&handler, &session, pane_id)
        .await
        .expect("the drag enters copy mode through the root binding");
    assert_eq!(
        dragged
            .selection_start
            .map(|position| (position.x, position.y)),
        Some((4, 1)),
        "the selection must be anchored at the press cell"
    );
    assert!(
        dragged.selection_present,
        "the drag must leave a selection present"
    );

    send(&handler, pid, &[RELEASE]).await;
    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "the release must end copy mode, as copy-pipe-and-cancel does"
    );

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected the copied buffer, got {shown:?}");
    };
    assert_eq!(
        response.command_output().stdout(),
        COPIED,
        "the copied content must be the text tmux 3.7b copies for these reports"
    );
    assert_eq!(
        handed_to_the_application(&handler, &target).await,
        Vec::<u8>::new(),
        "an application that asked for no mouse mode must be handed nothing"
    );
}

/// tmux 3.7b, application in 1002 + 1006: the three reports reach the application byte for byte,
/// nothing is copied and copy mode is never entered.
#[tokio::test]
async fn an_application_in_a_mouse_mode_is_handed_the_reports_and_nothing_is_copied() {
    let (handler, session, target, pane_id, pid, _control_rx) = fixture("g6-application").await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&session, 0, 0, APPLICATION_MODES)
            .expect("the pane application reports its mouse modes");
        state.start_pane_input_capture_for_test(&target);
    }

    send(&handler, pid, &[PRESS, DRAG, RELEASE]).await;

    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "an application in a mouse mode keeps the pane out of copy mode"
    );
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(
        matches!(shown, Response::Error(_)),
        "nothing may be copied in this branch, as tmux answers no buffers: got {shown:?}"
    );
    assert_eq!(
        handed_to_the_application(&handler, &target).await,
        FORWARDED,
        "the application must be handed the three reports byte for byte"
    );
}

/// tmux 3.7b, a second drag before the release: the anchor stays at the press cell and the end
/// follows the last drag, so more text is copied.
#[tokio::test]
async fn a_second_drag_extends_the_selection_before_the_copy() {
    let (handler, session, target, pane_id, pid, _control_rx) = fixture("g6-extend").await;
    let _ = &target;

    send(&handler, pid, &[PRESS, DRAG]).await;
    let first = copy_mode(&handler, &session, pane_id)
        .await
        .expect("the first drag enters copy mode");
    assert_eq!(
        first.selection_end.map(|position| (position.x, position.y)),
        Some((9, 2)),
        "the first drag ends at its own cell"
    );

    send(&handler, pid, &[DRAG_FAR]).await;
    let extended = copy_mode(&handler, &session, pane_id)
        .await
        .expect("the second drag stays in copy mode");
    assert_eq!(
        extended
            .selection_start
            .map(|position| (position.x, position.y)),
        Some((4, 1)),
        "the anchor must still be the press cell after a second drag"
    );
    assert_eq!(
        extended
            .selection_end
            .map(|position| (position.x, position.y)),
        Some((14, 2)),
        "the end must follow the last drag cell"
    );

    send(&handler, pid, &[RELEASE_FAR]).await;
    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "the release must end copy mode"
    );
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let Response::ShowBuffer(response) = shown else {
        panic!("expected the copied buffer, got {shown:?}");
    };
    assert_eq!(
        response.command_output().stdout(),
        COPIED_EXTENDED,
        "the copied content must be what tmux 3.7b copies after the second drag"
    );
}

/// tmux 3.7b, a bare click: a press and a release at the same cell never enter copy mode and copy
/// nothing at all.
#[tokio::test]
async fn a_bare_click_copies_nothing() {
    let (handler, session, _target, pane_id, pid, _control_rx) = fixture("g6-click").await;

    send(&handler, pid, &[PRESS, RELEASE_AT_PRESS]).await;

    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "a click without a drag must not enter copy mode"
    );
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(
        matches!(shown, Response::Error(_)),
        "a click must leave no buffer, as tmux answers no buffers: got {shown:?}"
    );
}

/// tmux 3.7b with the mouse option off: the same drag copies nothing and enters no mode, because
/// the reports are never routed to a binding.
#[tokio::test]
async fn the_mouse_option_off_copies_nothing() {
    let (handler, session, _target, pane_id, pid, _control_rx) = fixture("g6-mouse-off").await;
    let off = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(off, Response::SetOption(_)), "{off:?}");

    send(&handler, pid, &[PRESS, DRAG, RELEASE]).await;

    assert!(
        copy_mode(&handler, &session, pane_id).await.is_none(),
        "with the mouse option off no binding runs, so copy mode is never entered"
    );
    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(
        matches!(shown, Response::Error(_)),
        "nothing may be copied with the mouse option off: got {shown:?}"
    );
}
