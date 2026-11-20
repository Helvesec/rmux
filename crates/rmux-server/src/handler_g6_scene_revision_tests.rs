//! Mouse delivery through an ordinary scene revision.
//!
//! The obligation here is a scene change observed beyond identities: the same pane, the same
//! registered client and the same identifiers must deliver *differently* once the scene actually
//! changes, and deliver as before once it is restored.
//!
//! The revision is the product's own `Request::SetOption` on `OptionName::Status`, the same request
//! already driven for `OptionName::Mouse`. Nothing here writes geometry directly, invents a scene or
//! fabricates a revision counter: the product is not assumed to have one.
//!
//! What the source fixes before anything is measured:
//!
//! * `status_lines.rs` - `status_line_count` is 1 for the unset default and for `"on"`, and 0 for
//!   `"off"`.
//! * `mouse/layout.rs` - the status is disabled exactly when `Status` resolves to `Some("off")`;
//!   `StatusPosition` puts it at the top only for `Some("top")`, and the unset default therefore
//!   places it at the bottom; the content height is `window rows min (rows - status_lines)`.
//! * `handler_options.rs` - `OptionName::Status` is one of the options that requests a resize
//!   policy reconciliation, and the mutation answers `Response::SetOption` when it succeeds.
//!
//! So on an 80x24 session the content is 23 rows while the status line is on and 24 rows while it is
//! off, and client zero-based row 23 belongs to the content only in the second state. The status
//! position is taken from that resolve, not guessed from a membership probe.
//!
//! The level is the handler, its in-memory model and the pre-PTY addressing capture. No native
//! client, no render or viewport, no final application recipient and no parity is claimed.

use super::RequestHandler;
use rmux_core::input::mode::{MODE_CURSOR, MODE_MOUSE_SGR, MODE_MOUSE_STANDARD, MODE_WRAP};
use rmux_proto::{
    OptionName, PaneId, PaneTarget, Request, Response, ScopeSelector, SessionName, SetOptionMode,
    SetOptionRequest, TerminalSize,
};
use tokio::sync::mpsc;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const PID: u32 = 4642;

/// The WindowId, PaneId and canonical runtime session name of the scene, so that their equality
/// across the journey cannot stand in for the geometry having actually changed.
async fn scene_identity(
    handler: &RequestHandler,
    session: &SessionName,
) -> (rmux_core::WindowId, PaneId, SessionName) {
    let state = handler.state.lock().await;
    let window = state
        .sessions
        .session(session)
        .expect("the scene session")
        .window_at(0)
        .expect("the scene window");
    let pane = window.panes().first().expect("the scene pane").id();
    (
        window.id(),
        pane,
        state.runtime_session_name_for_window(session, 0),
    )
}

/// Drive the product's ordinary Status mutation and require its expected answer.
async fn set_status(handler: &RequestHandler, value: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(
        matches!(response, Response::SetOption(_)),
        "the ordinary Status mutation must return its expected response: {response:?}"
    );
}

/// Inject one coherent press/release pair through the live attached entry and return only what this
/// injection appended to the capture, so earlier content is never read as a new write.
async fn injected_delta(
    handler: &RequestHandler,
    target: &PaneTarget,
    press: &[u8],
    release: &[u8],
) -> Vec<u8> {
    let before = {
        let state = handler.state.lock().await;
        state
            .pane_input_capture_for_test(target)
            .expect("the capture is armed")
            .len()
    };
    for bytes in [press, release] {
        handler
            .handle_attached_live_input_for_test(PID, bytes)
            .await
            .expect("each half of the press/release pair must succeed");
    }
    let state = handler.state.lock().await;
    let captured = state
        .pane_input_capture_for_test(target)
        .expect("the capture is armed");
    captured[before..].to_vec()
}

/// The same live attach and the same pane deliver a boundary cell only while the scene really
/// contains it, and the restoration puts the scene back.
///
/// Each injection is a coherent press/release pair, and the interior positive moves one column per
/// phase so no forwarded click is ever repeated at a single cell. Interior presses use the left
/// button and boundary presses the middle one, so no two consecutive presses form a double or triple
/// click: the click timer runs on the real clock, and once it expires between a second click and its
/// release the DoubleClick1Pane binding sends the press again. Column 70 keeps the ignored phases
/// clear of the status line's window-list buttons, and the identity assertion would fail if a status
/// binding had switched the current window instead.
#[tokio::test]
async fn mouse_delivery_follows_the_scene_across_an_ordinary_status_change() {
    /// Client SGR(70,24) is zero-based (69,23): the status line while the status is on, and ordinary
    /// content once it is off, where it re-encodes to SGR(70,24).
    const BOUNDARY_PRESS: &[u8] = b"\x1b[<1;70;24M";
    const BOUNDARY_RELEASE: &[u8] = b"\x1b[<1;70;24m";
    const BOUNDARY_DELIVERED: &[u8] = b"\x1b[<1;70;24M\x1b[<1;70;24m";
    const NOTHING: &[u8] = b"";

    let session = SessionName::new("g6scene").expect("valid session name");
    let handler = RequestHandler::new();
    let (state, window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(&session);
    *handler.state.lock().await = state;

    let mouse = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Mouse,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(mouse, Response::SetOption(_)), "{mouse:?}");

    let (mode_before, mode_after) = {
        let mut state = handler.state.lock().await;
        state.g6_install_pane_transcript(&session, 0, pane_id, SIZE);
        state.g6_enable_pane_mouse_tracking(&session, 0, pane_id)
    };
    assert_eq!(
        mode_before,
        MODE_CURSOR | MODE_WRAP,
        "the ordinary screen starts with no mouse mode"
    );
    assert_eq!(
        mode_after,
        MODE_CURSOR | MODE_WRAP | MODE_MOUSE_STANDARD | MODE_MOUSE_SGR,
        "tracking is established by parsing DECSET output, not by writing bits"
    );

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(PID, session.clone(), control_tx)
        .await;
    let target = PaneTarget::with_window(session.clone(), 0, 0);
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&target);
    }
    let identity_before = scene_identity(&handler, &session).await;
    assert_eq!(
        (identity_before.0, identity_before.1),
        (window_id, pane_id),
        "the scene starts on the window and pane the fixture built"
    );

    // Phase A - the ordinary default: status on, one line, at the bottom. Content is 23 rows.
    assert_eq!(
        injected_delta(&handler, &target, b"\x1b[<0;5;2M", b"\x1b[<0;5;2m").await,
        b"\x1b[<0;5;2M\x1b[<0;5;2m",
        "phase A: an interior cell is delivered at its own pane-local position"
    );
    assert_eq!(
        injected_delta(&handler, &target, BOUNDARY_PRESS, BOUNDARY_RELEASE).await,
        NOTHING,
        "phase A: zero-based row 23 is the status line, so nothing is added to the capture"
    );

    // Phase B - the revision: status off. Content becomes 24 rows.
    set_status(&handler, "off").await;
    assert_eq!(
        injected_delta(&handler, &target, b"\x1b[<0;6;2M", b"\x1b[<0;6;2m").await,
        b"\x1b[<0;6;2M\x1b[<0;6;2m",
        "phase B: the interior positive still reaches the same pane, so entry and recipient work"
    );
    assert_eq!(
        injected_delta(&handler, &target, BOUNDARY_PRESS, BOUNDARY_RELEASE).await,
        BOUNDARY_DELIVERED,
        "phase B: the same cell is now content and is delivered at pane-local (69,23)"
    );

    // Phase C - the restoration: status on again.
    set_status(&handler, "on").await;
    assert_eq!(
        injected_delta(&handler, &target, b"\x1b[<0;7;2M", b"\x1b[<0;7;2m").await,
        b"\x1b[<0;7;2M\x1b[<0;7;2m",
        "phase C: the interior positive is unaffected by the restoration"
    );
    assert_eq!(
        injected_delta(&handler, &target, BOUNDARY_PRESS, BOUNDARY_RELEASE).await,
        NOTHING,
        "phase C: the restored status line takes the row back, so delivery follows the scene"
    );

    assert_eq!(
        scene_identity(&handler, &session).await,
        identity_before,
        "the window, pane and canonical runtime identity never changed; only the scene did"
    );
}
