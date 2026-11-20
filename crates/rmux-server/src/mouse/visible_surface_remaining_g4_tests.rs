//! The rest of the visible surface: the right scrollbar and its track, a
//! deeper vertical viewport, and a float whose logical origin is signed.
//!
//! The comparable cases were measured on tmux 3.7b before this module was
//! written, and the measured bytes are written out: `g4-r2/
//! ORACLE-R2.json` holds them, with the command and the sink of every reader.
//! Floats have no tmux oracle - tmux has none - so their expectations come
//! from the accepted Surface contract of a signed logical origin, and each is
//! stated as such.
//!
//! The level is the server's handler: an internal attach and the bytes written
//! toward the pane, captured before the PTY. That level alone closes no native
//! property, and none is claimed here.

use rmux_core::FloatingPaneRect;
use rmux_proto::{
    NewSessionRequest, OptionName, PaneTarget, Request, Response, ScopeSelector, SelectPaneRequest,
    SessionName, SetOptionMode, SetOptionRequest, SplitDirection, SplitWindowRequest,
    SplitWindowTarget, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

use super::RequestHandler;

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

/// Adds a second pane, so one of them can be floated over the other.
async fn second_pane(handler: &RequestHandler, session: &SessionName) {
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
}

/// Floats pane 1 at a signed rectangle of the window's content area. This is
/// the window's own placement API, not a creation command.
async fn float_pane_at(
    handler: &RequestHandler,
    session: &SessionName,
    x: i32,
    y: i32,
    cols: i32,
    rows: i32,
) {
    let target = WindowTarget::with_window(session.clone(), 0);
    let mut state = handler.state.lock().await;
    let mut window = state
        .sessions
        .window_layout_context_mut(&target)
        .expect("window layout context");
    window
        .float_pane(
            1,
            FloatingPaneRect::new(x, y, cols, rows).expect("valid floating rect"),
        )
        .expect("the pane floats at that rectangle");
}

/// Puts pane `index` in button tracking with SGR reports, through its own
/// output, and starts recording what RMUX writes back to it.
async fn application_asks_for_mouse(
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

const BUTTON_TRACKING: &[u8] = b"\x1b[?1002h\x1b[?1006h";
const ANY_EVENT_TRACKING: &[u8] = b"\x1b[?1003h\x1b[?1006h";

/// tmux 3.7b with the scrollbar on the right: the pane keeps columns 0-78 and
/// a click inside reaches the application with its column unchanged.
#[tokio::test]
async fn a_right_scrollbar_leaves_the_content_column_unchanged() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-sbright").await;
    set(&handler, OptionName::PaneScrollbars, "on").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "right").await;
    let target = application_asks_for_mouse(&handler, &session, 0, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<0;5;3M", b"\x1b[<0;5;3m"]).await;

    assert_eq!(
        received(&handler, &target).await,
        b"\x1b[<0;5;3M\x1b[<0;5;3m".to_vec(),
        "a scrollbar on the right does not move the content origin"
    );
}

/// tmux 3.7b hands no application a click on the right scrollbar itself.
#[tokio::test]
async fn a_click_on_the_right_scrollbar_reaches_no_application() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-sbright-bar").await;
    set(&handler, OptionName::PaneScrollbars, "on").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "right").await;
    let target = application_asks_for_mouse(&handler, &session, 0, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<0;80;3M", b"\x1b[<0;80;3m"]).await;

    assert!(
        received(&handler, &target).await.is_empty(),
        "the scrollbar column belongs to the scrollbar"
    );
}

/// tmux 3.7b gives a pane that asked for every event nothing at all when the
/// gesture is a drag along the scrollbar track.
#[tokio::test]
async fn a_drag_on_the_scrollbar_track_delivers_no_application_input() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-sbdrag").await;
    set(&handler, OptionName::PaneScrollbars, "on").await;
    set(&handler, OptionName::PaneScrollbarsPosition, "right").await;
    let target = application_asks_for_mouse(&handler, &session, 0, ANY_EVENT_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(
        &handler,
        pid,
        &[b"\x1b[<0;80;5M", b"\x1b[<32;80;9M", b"\x1b[<0;80;9m"],
    )
    .await;

    assert!(
        received(&handler, &target).await.is_empty(),
        "the whole gesture belongs to the scrollbar, including its motion and its release"
    );
}

/// tmux 3.7b with two status rows at the top: the row an application receives
/// is the client row minus two, and the same click keeps its row when those two
/// rows are at the bottom.
#[tokio::test]
async fn two_status_rows_shift_the_forwarded_row_by_two_only_at_the_top() {
    let handler = RequestHandler::new();
    let top = session(&handler, "g4r2-status2-top").await;
    set(&handler, OptionName::Status, "2").await;
    set(&handler, OptionName::StatusPosition, "top").await;
    let at_top = application_asks_for_mouse(&handler, &top, 0, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &top).await;

    send(&handler, pid, &[b"\x1b[<0;5;5M", b"\x1b[<0;5;5m"]).await;

    assert_eq!(
        received(&handler, &at_top).await,
        b"\x1b[<0;5;3M\x1b[<0;5;3m".to_vec(),
        "two status rows at the top move the first pane row down by two"
    );

    set(&handler, OptionName::StatusPosition, "bottom").await;
    let bottom = session(&handler, "g4r2-status2-bottom").await;
    set(&handler, OptionName::Status, "2").await;
    let at_bottom = application_asks_for_mouse(&handler, &bottom, 0, BUTTON_TRACKING).await;
    let (other_pid, _other_rx) = attach(&handler, &bottom).await;

    send(&handler, other_pid, &[b"\x1b[<0;5;5M", b"\x1b[<0;5;5m"]).await;

    assert_eq!(
        received(&handler, &at_bottom).await,
        b"\x1b[<0;5;5M\x1b[<0;5;5m".to_vec(),
        "the same two rows at the bottom shift nothing, which is what discriminates the offset"
    );
}

/// A float whose logical origin is negative: its first visible cell is not its
/// own (0,0).
///
/// The accepted Surface contract places a float at a signed origin in the
/// window's content area and clips it to the visible surface. A click on the
/// first visible cell of a float at (-3,-2) must therefore be delivered with
/// the local coordinates that origin implies, column 4 and row 3 counting from
/// one, and not with a coordinate fabricated at the cut. tmux has no floating
/// pane, so no tmux measurement can state this.
#[tokio::test]
async fn a_negative_origin_float_reports_the_cell_its_signed_origin_implies() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-float-negative").await;
    second_pane(&handler, &session).await;
    float_pane_at(&handler, &session, -3, -2, 30, 10).await;
    let tiled = application_asks_for_mouse(&handler, &session, 0, BUTTON_TRACKING).await;
    let float = application_asks_for_mouse(&handler, &session, 1, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    send(&handler, pid, &[b"\x1b[<0;1;1M", b"\x1b[<0;1;1m"]).await;

    assert_eq!(
        received(&handler, &float).await,
        b"\x1b[<0;4;3M\x1b[<0;4;3m".to_vec(),
        "the first visible cell of a float at (-3,-2) is its own column 4, row 3"
    );
    assert!(
        received(&handler, &tiled).await.is_empty(),
        "the pane the float covers there receives nothing"
    );
}

/// The same float, where it does not cover the tiled pane: that pane does
/// receive the click. Read with the case above, this is the positive and the
/// negative reader of one masking.
#[tokio::test]
async fn the_pane_under_a_float_receives_what_the_float_does_not_cover() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-float-uncovered").await;
    second_pane(&handler, &session).await;
    float_pane_at(&handler, &session, -3, -2, 30, 10).await;
    let tiled = application_asks_for_mouse(&handler, &session, 0, BUTTON_TRACKING).await;
    let float = application_asks_for_mouse(&handler, &session, 1, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    // Column 60 is outside the float, which ends at content column 26.
    send(&handler, pid, &[b"\x1b[<0;60;10M", b"\x1b[<0;60;10m"]).await;

    assert_eq!(
        received(&handler, &tiled).await,
        b"\x1b[<0;60;10M\x1b[<0;60;10m".to_vec(),
        "outside the float the tiled pane keeps receiving its own clicks"
    );
    assert!(
        received(&handler, &float).await.is_empty(),
        "and the float receives nothing where it is not"
    );
}

/// A float clipped at the bottom: its last visible row is delivered with the
/// row its origin implies, and nothing below the visible surface reaches any
/// application.
#[tokio::test]
async fn a_bottom_clipped_float_reports_its_last_visible_row_and_nothing_below() {
    let handler = RequestHandler::new();
    let session = session(&handler, "g4r2-float-clipped").await;
    second_pane(&handler, &session).await;
    // The window's content is 23 rows; this float asks for rows 18 to 27.
    float_pane_at(&handler, &session, 10, 18, 20, 10).await;
    let tiled = application_asks_for_mouse(&handler, &session, 0, BUTTON_TRACKING).await;
    let float = application_asks_for_mouse(&handler, &session, 1, BUTTON_TRACKING).await;
    let (pid, _control_rx) = attach(&handler, &session).await;

    // Client row 23 is content row 22, the float's last visible row.
    send(&handler, pid, &[b"\x1b[<0;12;23M", b"\x1b[<0;12;23m"]).await;

    assert_eq!(
        received(&handler, &float).await,
        b"\x1b[<0;2;5M\x1b[<0;2;5m".to_vec(),
        "the last visible row of the float is its own row 5, counted from its origin at 18"
    );
    assert!(
        received(&handler, &tiled).await.is_empty(),
        "the pane under that cell receives nothing"
    );

    // Client row 24 is the status line: below every pane.
    send(&handler, pid, &[b"\x1b[<0;12;24M", b"\x1b[<0;12;24m"]).await;

    assert_eq!(
        received(&handler, &float).await,
        b"\x1b[<0;2;5M\x1b[<0;2;5m".to_vec(),
        "a click below the visible surface adds nothing to the float"
    );
    assert!(
        received(&handler, &tiled).await.is_empty(),
        "and nothing to the pane either"
    );
}
