//! Where a click on a really visible surface lands: which pane, and with which
//! local coordinates.
//!
//! Every expectation was measured on tmux 3.7b before this module was written,
//! with a pane application in raw mode asking for 1002 and 1006 and recording
//! what it was handed; the measurements are in `g4/ORACLE-G4.json`.
//! RMUX's own output is never the oracle, so the expected bytes are written
//! out.
//!
//! The level is named: this is the server's handler, a real attach and the
//! bytes written toward the pane. No client rendering and no native reader are
//! exercised here.

use rmux_proto::{
    NewSessionRequest, OptionName, PaneTarget, Request, Response, ScopeSelector, SelectPaneRequest,
    SessionName, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, TerminalSize,
};
use tokio::sync::mpsc;

use super::RequestHandler;

/// Button 1 pressed and released on one cell, as a terminal reports it.
const PRESS_5_3: &[u8] = b"\x1b[<0;5;3M";
const RELEASE_5_3: &[u8] = b"\x1b[<0;5;3m";

async fn session(handler: &RequestHandler, name: &str) -> SessionName {
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
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    set(handler, OptionName::Mouse, "on").await;
    session
}

async fn set(handler: &RequestHandler, option: OptionName, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Puts pane `index` in button tracking with SGR reports, through its own
/// output, and starts recording what RMUX writes back to it.
async fn application_asks_for_mouse(
    handler: &RequestHandler,
    session: &SessionName,
    index: u32,
) -> PaneTarget {
    let target = PaneTarget::with_window(session.clone(), 0, index);
    let mut state = handler.state.lock().await;
    state
        .append_bytes_to_pane_transcript_for_test(session, 0, index, b"\x1b[?1002h\x1b[?1006h")
        .expect("the pane reports its mouse modes");
    state.start_pane_input_capture_for_test(&target);
    target
}

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

/// The first client row this pane answers for, as the mouse path itself sees
/// it: the rectangle a click is translated against, probed through the same
/// accessor the pane encoder uses.
async fn pane_first_client_row(
    handler: &RequestHandler,
    target: &PaneTarget,
    column: u16,
) -> Option<u16> {
    let state = handler.state.lock().await;
    let geometry = crate::mouse::pane_mouse_coordinates_for_target(&state, target)?;
    (0..64).find(|row| geometry.relative_position(column, *row).is_some())
}

async fn received(handler: &RequestHandler, target: &PaneTarget) -> Vec<u8> {
    let state = handler.state.lock().await;
    state
        .pane_input_capture_for_test(target)
        .expect("the pane input capture is running")
}

/// tmux 3.7b with scrollbars always on and placed left: the pane starts at
/// column 1 and a click at client column 5 reaches the application as column 4.
#[tokio::test]
async fn left_scrollbar_excludes_its_column_from_the_forwarded_coordinates() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4-scrollbar-left").await;
    set(&handler, OptionName::PaneScrollbars, "on").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "left").await;
    let target = application_asks_for_mouse(&handler, &session, 0).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[PRESS_5_3, RELEASE_5_3]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;4;3M\x1b[<0;4;3m".to_vec(),
        "the scrollbar column is not part of the content coordinates"
    );
}

/// tmux 3.7b with modal scrollbars: an ordinary pane keeps the whole width, so
/// the same click reaches the application as column 5.
#[tokio::test]
async fn modal_scrollbars_do_not_shift_an_ordinary_pane() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4-scrollbar-modal").await;
    set(&handler, OptionName::PaneScrollbars, "modal").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "left").await;
    let target = application_asks_for_mouse(&handler, &session, 0).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[PRESS_5_3, RELEASE_5_3]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<0;5;3m".to_vec(),
        "a pane that shows no scrollbar keeps its own origin"
    );
}

/// tmux 3.7b hands no application a click on the scrollbar itself: it keeps it
/// for its own scrollbar bindings, and no coordinate is fabricated for the
/// pane.
#[tokio::test]
async fn a_click_on_the_scrollbar_track_reaches_no_application() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4-scrollbar-track").await;
    set(&handler, OptionName::PaneScrollbars, "on").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "left").await;
    let target = application_asks_for_mouse(&handler, &session, 0).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<0;1;3M", b"\x1b[<0;1;3m"]).await;

    assert!(
        received(&handler, &target).await.is_empty(),
        "the scrollbar column belongs to the scrollbar, not to the application"
    );
}

/// tmux 3.7b with the status line at the top: the row an application receives
/// is the client row minus the status line.
#[tokio::test]
async fn a_status_line_at_the_top_shifts_the_forwarded_row() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4-status-top").await;
    set(&handler, OptionName::StatusPosition, "top").await;
    let target = application_asks_for_mouse(&handler, &session, 0).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[PRESS_5_3, RELEASE_5_3]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;2M\x1b[<0;5;2m".to_vec(),
        "the status line at the top moves the pane's first row down by one"
    );
}

/// A click in the lower pane of a vertical split is translated into that
/// pane's own rows, exactly.
///
/// tmux 3.7b places the two panes of an 80x24 window with a bottom status line
/// at rows 0-10 and 12-22, border on row 11, and a click at client row 15
/// reaches the lower application as row 3. RMUX places its lower pane one row
/// further down, so this witness reads the rectangle the mouse path really
/// uses and requires the translation to be exact against it: that is the G4
/// property. The placement difference is a layout question, on a path this
/// lane may not edit, and is reported as its own observation rather than
/// frozen here as an expectation.
#[tokio::test]
async fn a_click_in_the_lower_pane_arrives_in_that_pane_local_rows() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4-vsplit").await;
    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(session.clone()),
            direction: SplitDirection::Vertical,
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

    let upper = application_asks_for_mouse(&handler, &session, 0).await;
    let lower = application_asks_for_mouse(&handler, &session, 1).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    let upper_top = pane_first_client_row(&handler, &upper, 7)
        .await
        .expect("the upper pane answers for some row");
    let lower_top = pane_first_client_row(&handler, &lower, 7)
        .await
        .expect("the lower pane answers for some row");
    println!(
        "vertical split: upper starts at client row {upper_top}, lower at {lower_top}; \
         tmux 3.7b measured 0 and 12"
    );
    assert_eq!(
        upper_top, 0,
        "the upper pane starts at the top of the window"
    );
    assert!(
        lower_top > upper_top,
        "the lower pane starts below the upper one: {upper_top} then {lower_top}"
    );

    // Two rows inside the lower pane, named from the rectangle it really has.
    let client_row = lower_top + 2;
    let press = format!("\x1b[<0;7;{}M", client_row + 1).into_bytes();
    let release = format!("\x1b[<0;7;{}m", client_row + 1).into_bytes();

    send(&handler, pid, &[&press, &release]).await;

    let expected = b"\x1b[<0;7;3M\x1b[<0;7;3m".to_vec();
    assert_eq!(
        received(&handler, &lower).await,
        expected,
        "a click two rows inside the lower pane arrives there as its own row 3, column unchanged"
    );
    assert!(
        received(&handler, &upper).await.is_empty(),
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
        "the click activates the pane it landed in"
    );
}
