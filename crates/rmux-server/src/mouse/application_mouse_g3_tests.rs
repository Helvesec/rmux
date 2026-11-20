//! What an application running in a pane receives when a real attached client
//! moves its mouse over the visible surface.
//!
//! Every expectation here was measured on tmux 3.7b before this module was
//! written, with a pane application that put its terminal in raw mode, asked
//! for the tracking mode under test and recorded the bytes it was given; the
//! measurements and the method are in `g3/ORACLE.json`. RMUX's own
//! encoder is never the oracle, so the expected bytes are written out.
//!
//! tmux forwards those reports through its default root bindings
//! (`MouseDown1Pane` = `select-pane -t = ; send-keys -M`, `MouseDrag1Pane` and
//! `WheelUpPane` forwarding when `#{mouse_any_flag}` is set, and the double and
//! triple click bindings forwarding the same way), which is why an application
//! that asked for mouse reports receives the event instead of tmux's own
//! copy-mode reaction.

use rmux_proto::{
    NewSessionRequest, OptionName, PaneTarget, Request, Response, ScopeSelector, SelectPaneRequest,
    SessionName, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, TerminalSize,
};
use tokio::sync::mpsc;

use super::RequestHandler;

/// A client report: what the terminal sends to RMUX.
const PRESS: &[u8] = b"\x1b[<0;5;3M";
const DRAG: &[u8] = b"\x1b[<32;6;4M";
const RELEASE_AT_DRAG: &[u8] = b"\x1b[<0;6;4m";
const RELEASE_AT_PRESS: &[u8] = b"\x1b[<0;5;3m";
const BARE_MOTION: &[u8] = b"\x1b[<35;7;5M";
const WHEEL_UP: &[u8] = b"\x1b[<64;8;6M";

async fn session_with_mouse(handler: &RequestHandler, name: &str, cols: u16) -> SessionName {
    let session = SessionName::new(name).expect("valid session name");
    {
        let mut state = handler.state.lock().await;
        state
            .options
            .set(
                ScopeSelector::Global,
                OptionName::DefaultShell,
                "/bin/bash".to_owned(),
                SetOptionMode::Replace,
            )
            .expect("test default-shell is valid");
    }
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let mouse = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(mouse, Response::SetOption(_)), "{mouse:?}");
    session
}

/// Puts pane `index` in the given DEC modes through its own output, as an
/// application does, and starts recording what RMUX writes back to it.
async fn application_asks_for(
    handler: &RequestHandler,
    session: &SessionName,
    index: u32,
    modes: &[u8],
) -> PaneTarget {
    let target = PaneTarget::with_window(session.clone(), 0, index);
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, index, modes)
        .expect("the pane reports its mouse modes");
    state.start_pane_input_capture_for_test(&target);
    target
}

/// Registers an attach and hands its control receiver back: the attach is only
/// alive while that receiver is, so each observation keeps it for its own
/// duration.
async fn attach(
    handler: &RequestHandler,
    session: &SessionName,
) -> (u32, mpsc::UnboundedReceiver<crate::pane_io::AttachControl>) {
    let requester_pid = std::process::id();
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, session.clone(), control_tx)
        .await;
    (requester_pid, control_rx)
}

async fn send(handler: &RequestHandler, pid: u32, reports: &[&[u8]]) {
    for report in reports {
        handler
            .handle_attached_live_input_for_test(pid, report)
            .await
            .expect("live attach mouse input");
    }
}

async fn received(handler: &RequestHandler, target: &PaneTarget) -> Vec<u8> {
    let state = handler.state.lock().await;
    state
        .pane_input_capture_for_test(target)
        .expect("the pane input capture is running")
}

/// tmux 3.7b, pane in 1000 + 1006: the drag is not forwarded, the press, the
/// wheel and the release are.
#[tokio::test]
async fn standard_tracking_drops_a_drag_and_forwards_press_wheel_and_release() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-standard", 80).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1000h\x1b[?1006h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[PRESS, DRAG, WHEEL_UP, RELEASE_AT_PRESS]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<64;8;6M\x1b[<0;5;3m".to_vec(),
        "a drag needs a motion mode; tmux 3.7b forwards only the press, the wheel and the release"
    );
}

/// tmux 3.7b, pane in 1002 + 1006: the drag is forwarded, the bare motion -
/// button bits 3, no button held - is not.
#[tokio::test]
async fn button_tracking_forwards_a_drag_and_drops_a_bare_motion() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-button", 80).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1002h\x1b[?1006h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(
        &handler,
        pid,
        &[PRESS, DRAG, RELEASE_AT_DRAG, BARE_MOTION, WHEEL_UP],
    )
    .await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<32;6;4M\x1b[<0;6;4m\x1b[<64;8;6M".to_vec(),
        "button tracking carries the drag and stops at the bare motion"
    );
}

/// tmux 3.7b, pane in 1003 + 1006: the same five reports, including the bare
/// motion.
#[tokio::test]
async fn any_event_tracking_forwards_the_bare_motion() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-any", 80).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1003h\x1b[?1006h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(
        &handler,
        pid,
        &[PRESS, DRAG, RELEASE_AT_DRAG, BARE_MOTION, WHEEL_UP],
    )
    .await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<32;6;4M\x1b[<0;6;4m\x1b[<35;7;5M\x1b[<64;8;6M".to_vec(),
        "any-event tracking adds the bare motion to what button tracking carries"
    );
}

/// tmux 3.7b, pane in 1002 without 1006: ESC [ M then button+0x20, column+0x21
/// and row+0x21, an SGR release arriving as button 3, and the wheel as 0x60.
#[tokio::test]
async fn button_tracking_legacy_encoding_matches_the_measured_bytes() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-legacy", 80).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1002h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(
        &handler,
        pid,
        &[PRESS, DRAG, RELEASE_AT_DRAG, BARE_MOTION, WHEEL_UP],
    )
    .await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[M\x20\x25\x23\x1b[M\x40\x26\x24\x1b[M\x23\x26\x24\x1b[M\x60\x28\x26".to_vec(),
        "the legacy frames measured on tmux 3.7b, in order, with the bare motion still dropped"
    );
}

/// tmux 3.7b, pane in 1002 + 1005 on a wide client: a column beyond 127 is
/// written as two UTF-8 bytes.
#[tokio::test]
async fn button_tracking_utf8_encoding_matches_the_measured_bytes() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-utf8", 220).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1002h\x1b[?1005h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<0;200;3M", b"\x1b[<0;200;3m"]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[M\x20\xc3\xa8\x23\x1b[M\x23\xc3\xa8\x23".to_vec(),
        "column 200 is 199 + 33 = 232, which UTF-8 mode writes as 0xc3 0xa8"
    );
}

/// tmux 3.7b forwards every press and release of a fast repeat, in order: an
/// application that asked for mouse reports sees the clicks themselves, not a
/// double or triple click tmux resolved for itself.
#[tokio::test]
async fn repeated_clicks_are_forwarded_one_by_one() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-clicks", 80).await;
    let target = application_asks_for(&handler, &session, 0, b"\x1b[?1002h\x1b[?1006h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(
        &handler,
        pid,
        &[
            PRESS,
            RELEASE_AT_PRESS,
            PRESS,
            RELEASE_AT_PRESS,
            PRESS,
            RELEASE_AT_PRESS,
        ],
    )
    .await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<0;5;3M\x1b[<0;5;3m\x1b[<0;5;3M\x1b[<0;5;3m".to_vec(),
        "three presses and three releases, in the order they were reported"
    );
}

/// tmux 3.7b with focus-follows-mouse on: a bare motion over the other pane
/// moves the active pane there and, because that pane's application asked for
/// motion, hands it the report in the pane's own coordinates - client column
/// 60 over a pane that starts at column 41 becomes column 19. The pane that was
/// not hovered receives nothing.
#[tokio::test]
async fn focus_follows_mouse_moves_focus_and_forwards_the_motion_in_pane_coordinates() {
    let handler = RequestHandler::new();
    let session = session_with_mouse(&handler, "g3-ffm", 80).await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)), "{split:?}");
    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(session.clone(), 0),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert!(matches!(selected, Response::SelectPane(_)), "{selected:?}");
    let follows = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::FocusFollowsMouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(follows, Response::SetOption(_)), "{follows:?}");

    let left = application_asks_for(&handler, &session, 0, b"\x1b[?1003h\x1b[?1006h").await;
    let right = application_asks_for(&handler, &session, 1, b"\x1b[?1003h\x1b[?1006h").await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<35;60;5M"]).await;

    assert_eq!(
        received(&handler, &right).await,
        b"\x1b[<35;19;5M".to_vec(),
        "the hovered pane receives the motion in its own frame"
    );
    assert!(
        received(&handler, &left).await.is_empty(),
        "the pane the pointer is not over receives nothing"
    );

    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(&session)
        .and_then(|session| session.window_at(0))
        .expect("window 0 exists");
    let hovered = window.pane(1).expect("pane 1 exists").id();
    assert_eq!(
        window.active_pane().map(|pane| pane.id()),
        Some(hovered),
        "focus-follows-mouse moves the active pane to the hovered one"
    );
}
