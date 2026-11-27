//! Two winlinks of one window, made by the real ordinary link through the complete handler.
//!
//! A link here is a winlink: the same window addressed at two `(session, window index)` slots. This
//! observation never writes a second winlink by hand and never calls the core store to make it: it
//! issues `Request::LinkWindow` and lets the whole handler run, lifecycle preparation included.
//!
//! The state comes from `HandlerState::g6_multiple_links_fixture`, which is process-free and built
//! from ordinary product constructors. The level reached is the handler and its in-memory model: no
//! client, no PTY, no render and no application, so nothing here is a native observation.

use super::RequestHandler;
use rmux_core::input::mode::{
    ALL_MOUSE_MODES, MODE_CURSOR, MODE_MOUSE_SGR, MODE_MOUSE_STANDARD, MODE_WRAP,
};
use rmux_proto::{
    LinkWindowRequest, OptionName, PaneId, PaneTarget, Request, Response, ScopeSelector,
    SelectWindowRequest, SessionName, SetOptionMode, SetOptionRequest, TerminalSize, WindowTarget,
};
use tokio::sync::mpsc;

const FREE_INDEX: u32 = 7;
const NEVER_LINKED_INDEX: u32 = 3;

/// The two origins used by the mouse witness, kept at the sizes R5 established.
const WIDE_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const NARROW_SIZE: TerminalSize = TerminalSize { cols: 20, rows: 10 };
const MOUSE_WIDE_PID: u32 = 4542;
const MOUSE_NARROW_PID: u32 = 4543;

async fn linked_handler(name: &str) -> (RequestHandler, SessionName, rmux_core::WindowId, PaneId) {
    let session = SessionName::new(name).expect("valid session name");
    let handler = RequestHandler::new();
    let (state, window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(&session);
    *handler.state.lock().await = state;
    (handler, session, window_id, pane_id)
}

/// The ordinary link: an existing window to a free index of the same session, nothing replaced,
/// removed or shifted.
fn ordinary_link(session: &SessionName) -> Request {
    Request::LinkWindow(LinkWindowRequest {
        source: WindowTarget::with_window(session.clone(), 0),
        target: WindowTarget::with_window(session.clone(), FREE_INDEX),
        after: false,
        before: false,
        kill_destination: false,
        detached: true,
    })
}

#[tokio::test]
async fn the_ordinary_link_gives_two_winlinks_of_one_window_and_creates_nothing() {
    let (handler, session, window_id, pane_id) = linked_handler("g6-links").await;
    {
        let state = handler.state.lock().await;
        let live = state.sessions.session(&session).expect("session exists");
        assert!(live.window_at(0).is_some(), "the source winlink exists");
        assert!(
            live.window_at(FREE_INDEX).is_none(),
            "the destination index is free before the link"
        );
    }

    let response = handler.handle(ordinary_link(&session)).await;
    assert!(
        matches!(response, Response::LinkWindow(_)),
        "the complete handler must perform the ordinary link: {response:?}"
    );

    let state = handler.state.lock().await;
    let live = state.sessions.session(&session).expect("session exists");
    let source = live.window_at(0).expect("the source winlink survives");
    let linked = live
        .window_at(FREE_INDEX)
        .expect("the second winlink exists");
    assert_eq!(
        source.id(),
        linked.id(),
        "both winlinks must address the same window"
    );
    assert_eq!(source.id(), window_id, "and it is the window that existed");
    let source_panes = source
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();
    let linked_panes = linked
        .panes()
        .iter()
        .map(|pane| pane.id())
        .collect::<Vec<_>>();
    assert_eq!(
        source_panes, linked_panes,
        "the two winlinks must expose the same pane ids"
    );
    assert_eq!(
        source_panes,
        vec![pane_id],
        "and the link created no pane: the one pane is the one that existed"
    );
}

#[tokio::test]
async fn only_a_real_winlink_resolves_its_own_index() {
    let (handler, session, _window_id, pane_id) = linked_handler("g6-links-2").await;
    {
        let state = handler.state.lock().await;
        let live = state.sessions.session(&session).expect("session exists");
        assert!(
            live.window_at(FREE_INDEX).is_none(),
            "nothing answers for the free index before the link"
        );
    }

    let response = handler.handle(ordinary_link(&session)).await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let live = state.sessions.session(&session).expect("session exists");
    for index in [0, FREE_INDEX] {
        let window = live
            .window_at(index)
            .unwrap_or_else(|| panic!("link {index} resolves after the ordinary link"));
        assert_eq!(
            window.panes().first().expect("pane exists").id(),
            pane_id,
            "link {index} resolves to the pane of the window it addresses"
        );
    }
    assert!(
        live.window_at(NEVER_LINKED_INDEX).is_none(),
        "an index that was never linked must resolve to nothing, which is what separates a \
         per-link resolution from a global one"
    );
}

/// What the mouse path itself resolves for each link: the geometry it would translate a click
/// against, taken through the same accessor the pane encoder uses. A resolution that ignored the
/// addressed winlink would answer for an index that was never linked.
#[tokio::test]
async fn the_mouse_path_resolves_each_winlink_and_refuses_an_unlinked_index() {
    let (handler, session, _window_id, _pane_id) = linked_handler("g6-links-3").await;
    let response = handler.handle(ordinary_link(&session)).await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    let state = handler.state.lock().await;
    let resolved = |index: u32| {
        crate::mouse::pane_mouse_coordinates_for_target(
            &state,
            &PaneTarget::with_window(session.clone(), index, 0),
        )
        .is_some()
    };
    let link_zero = resolved(0);
    let link_free = resolved(FREE_INDEX);
    let never_linked = resolved(NEVER_LINKED_INDEX);
    assert!(
        link_zero && link_free,
        "the mouse path must resolve a geometry for each winlink of the window: link 0 gave \
         {link_zero}, link {FREE_INDEX} gave {link_free}"
    );
    assert!(
        !never_linked,
        "an index that was never linked must resolve to nothing in the mouse path, which is what \
         separates a per-link resolution from a global one"
    );
}

/// The A-01 observation: one window linked into a second session of a *different size*, so that its
/// two valid origins resolve different rectangles.
///
/// R4 could not discriminate because its two winlinks lived in one session and were therefore
/// geometrically identical by construction: a resolver that merely checked that the requested index
/// exists and then always used the source context would have satisfied every R4 assertion. Here the
/// expectation comes from the contract, not from the new output — an event arriving through a link
/// is translated against *that link's* rectangle — and the negatives are client cells that lie
/// inside the wide origin and outside the narrow one. Confusing the two contexts fails this test.
#[tokio::test]
async fn an_event_through_each_link_resolves_that_link_local_coordinates() {
    const NARROW_COLS: u16 = 20;
    const NARROW_ROWS: u16 = 10;
    /// Inside both rectangles.
    const SHARED_CELL: (u16, u16) = (5, 1);
    /// Inside the wide rectangle only: past the narrow link's last column.
    const BEYOND_NARROW_COLUMN: (u16, u16) = (30, 1);
    /// Inside the wide rectangle only: past the narrow link's last row.
    const BEYOND_NARROW_ROW: (u16, u16) = (5, 15);

    let wide = SessionName::new("g6r5-wide").expect("valid session name");
    let narrow = SessionName::new("g6r5-narrow").expect("valid session name");
    let handler = RequestHandler::new();
    let (mut state, window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(&wide);
    let narrow_pane = state.g6_add_sized_session(
        &narrow,
        TerminalSize {
            cols: NARROW_COLS,
            rows: NARROW_ROWS,
        },
    );
    assert_ne!(
        narrow_pane, pane_id,
        "the second session must own its own pane, so the link is the only thing they share"
    );
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

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(wide.clone(), 0),
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(
        matches!(linked, Response::LinkWindow(_)),
        "the ordinary link across the two sessions must succeed: {linked:?}"
    );

    {
        let state = handler.state.lock().await;
        let here = state.sessions.session(&wide).expect("wide session");
        let there = state.sessions.session(&narrow).expect("narrow session");
        assert_eq!(
            here.window_at(0).expect("source winlink").id(),
            window_id,
            "the source winlink still addresses the original window"
        );
        assert_eq!(
            there.window_at(FREE_INDEX).expect("linked winlink").id(),
            window_id,
            "and so does the winlink installed in the other session"
        );
        assert_eq!(
            there
                .window_at(FREE_INDEX)
                .expect("linked winlink")
                .panes()
                .first()
                .expect("the linked winlink keeps the window panes")
                .id(),
            pane_id,
            "both origins address one pane, which is why only the origin can disambiguate them"
        );
    }

    // Give the narrow session's client the linked winlink as its current window, so each attached
    // client is a real recipient of one origin of the same window.
    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "the linked winlink must be selectable in its own session: {selected:?}"
    );

    let (wide_tx, _wide_rx) = mpsc::unbounded_channel();
    let (narrow_tx, _narrow_rx) = mpsc::unbounded_channel();
    let wide_pid = 4242u32;
    let narrow_pid = 4243u32;
    handler
        .register_attach(wide_pid, wide.clone(), wide_tx)
        .await;
    handler
        .register_attach(narrow_pid, narrow.clone(), narrow_tx)
        .await;
    let wide_client = handler.active_attach_identity_for_test(wide_pid).await;
    let narrow_client = handler.active_attach_identity_for_test(narrow_pid).await;
    assert_ne!(
        wide_client.attach_pid(),
        narrow_client.attach_pid(),
        "the two origins have distinct recipients"
    );

    let state = handler.state.lock().await;
    let through_wide = crate::mouse::pane_mouse_coordinates_for_target(
        &state,
        &PaneTarget::with_window(wide.clone(), 0, 0),
    )
    .expect("the source winlink resolves its own geometry");
    let through_narrow = crate::mouse::pane_mouse_coordinates_for_target(
        &state,
        &PaneTarget::with_window(narrow.clone(), FREE_INDEX, 0),
    )
    .expect("the linked winlink resolves its own geometry");

    let (shared_column, shared_row) = SHARED_CELL;
    assert_eq!(
        through_wide.relative_position(shared_column, shared_row),
        through_narrow.relative_position(shared_column, shared_row),
        "a cell inside both rectangles lands on the same pane position through either origin"
    );
    assert!(
        through_wide
            .relative_position(shared_column, shared_row)
            .is_some(),
        "and it really is inside both"
    );

    for (column, row) in [BEYOND_NARROW_COLUMN, BEYOND_NARROW_ROW] {
        assert!(
            through_wide.relative_position(column, row).is_some(),
            "client cell ({column}, {row}) is inside the wide origin"
        );
        assert!(
            through_narrow.relative_position(column, row).is_none(),
            "client cell ({column}, {row}) is outside the narrow origin; resolving it means the \
             linked winlink was translated against the source context instead of its own"
        );
    }
}

/// The canonical identity fact R5 got wrong, as an assertion rather than prose.
///
/// `transcript_handle` keys `HandlerState::transcripts` by
/// `runtime_session_name_for_window(session, window_index)`, which resolves a window belonging to a
/// link group to that group's own runtime session name. So the two valid origins of one linked
/// window share ONE canonical transcript identity, while an unlinked window of the same session does
/// not - which is why a `terminals` entry never established a `transcripts` entry.
#[tokio::test]
async fn both_origins_of_a_linked_window_share_one_canonical_transcript_identity() {
    let wide = SessionName::new("g6r6id-wide").expect("valid session name");
    let narrow = SessionName::new("g6r6id-narrow").expect("valid session name");
    let handler = RequestHandler::new();
    let (mut state, _window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(&wide);
    state.g6_add_sized_session(&narrow, TerminalSize { cols: 20, rows: 10 });
    *handler.state.lock().await = state;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(wide.clone(), 0),
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let state = handler.state.lock().await;
    let through_source = state.runtime_session_name_for_window(&wide, 0);
    let through_link = state.runtime_session_name_for_window(&narrow, FREE_INDEX);
    assert_eq!(
        through_source, through_link,
        "both origins of the linked window resolve to one canonical runtime session name"
    );
    let narrows_own_window = state.runtime_session_name_for_window(&narrow, 0);
    assert_ne!(
        narrows_own_window, through_link,
        "an unlinked window of the same session does not share that canonical identity"
    );
    let _ = pane_id;
}

/// A-01 at route level: a real event injected through each attached identity, required to succeed
/// and to land in that origin's own capture, taken before any PTY sink.
///
/// The capture map is keyed by `target.to_string()`, so each origin of the one shared pane has its
/// own bucket. Injecting through a single identity and requiring the other bucket to stay empty is
/// therefore a routing negative: if the live route resolved the wrong one of two *valid* origins,
/// the bytes would land in the wrong bucket. Nothing here hands a `PaneTarget` to the resolver - the
/// route derives it from the attached identity itself, and the expected bytes are the injected bytes,
/// not a second output.
#[tokio::test]
async fn a_real_event_through_each_link_is_captured_before_the_pty_for_that_origin() {
    const WIDE_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
    const NARROW_SIZE: TerminalSize = TerminalSize { cols: 20, rows: 10 };
    const WIDE_INPUT: &[u8] = b"W";
    const NARROW_INPUT: &[u8] = b"N";
    const WIDE_PID: u32 = 4342;
    const NARROW_PID: u32 = 4343;

    let wide = SessionName::new("g6r6-wide").expect("valid session name");
    let narrow = SessionName::new("g6r6-narrow").expect("valid session name");
    let handler = RequestHandler::new();
    let (mut state, _window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(&wide);
    state.g6_add_sized_session(&narrow, NARROW_SIZE);
    *handler.state.lock().await = state;

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(wide.clone(), 0),
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    // The transcript goes in after the link, so its key is the canonical identity the link group
    // established rather than a name guessed before it existed.
    let canonical = {
        let mut state = handler.state.lock().await;
        state.g6_install_pane_transcript(&wide, 0, pane_id, WIDE_SIZE)
    };

    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "{selected:?}"
    );

    let wide_target = PaneTarget::with_window(wide.clone(), 0, 0);
    let narrow_target = PaneTarget::with_window(narrow.clone(), FREE_INDEX, 0);

    let (wide_tx, _wide_rx) = mpsc::unbounded_channel();
    let (narrow_tx, _narrow_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(WIDE_PID, wide.clone(), wide_tx)
        .await;
    handler
        .register_attach(NARROW_PID, narrow.clone(), narrow_tx)
        .await;
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&wide_target);
        state.start_pane_input_capture_for_test(&narrow_target);
    }

    handler
        .handle_attached_live_input_for_test(WIDE_PID, WIDE_INPUT)
        .await
        .expect("the event injected through the source winlink must succeed");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&wide_target).as_deref(),
            Some(WIDE_INPUT),
            "the source origin captured exactly what was injected through it, before any PTY sink"
        );
        assert_eq!(
            state.pane_input_capture_for_test(&narrow_target).as_deref(),
            Some(&[][..]),
            "routing negative: nothing was injected through the linked origin, so its own capture \
             must still be empty; bytes here would mean the route resolved the wrong valid origin"
        );
    }

    handler
        .handle_attached_live_input_for_test(NARROW_PID, NARROW_INPUT)
        .await
        .expect("the event injected through the linked winlink must succeed");
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(&narrow_target).as_deref(),
            Some(NARROW_INPUT),
            "the linked origin captured exactly what was injected through it"
        );
        assert_eq!(
            state.pane_input_capture_for_test(&wide_target).as_deref(),
            Some(WIDE_INPUT),
            "and the source origin's capture was not appended to by the other origin's event"
        );
    }
    assert_eq!(
        canonical,
        {
            let state = handler.state.lock().await;
            state.runtime_session_name_for_window(&narrow, FREE_INDEX)
        },
        "the transcript both origins used is the one canonical runtime identity of the link group"
    );
}

/// One ordinary scene for the mouse witness: the real link between an 80x24 and a 20x10 session, the
/// canonical transcript with mouse tracking established by parsing output bytes, the linked winlink
/// selected in the narrow session, and one registered client per origin with its own armed capture.
///
/// The two receivers are returned so the registered clients stay live for the whole observation.
async fn mouse_scene(
    wide: &SessionName,
    narrow: &SessionName,
) -> (
    RequestHandler,
    (u32, u32),
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
    mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let (mut state, _window_id, pane_id) =
        crate::pane_terminals::HandlerState::g6_multiple_links_fixture(wide);
    state.g6_add_sized_session(narrow, NARROW_SIZE);
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

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(wide.clone(), 0),
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");

    let modes = {
        let mut state = handler.state.lock().await;
        state.g6_install_pane_transcript(wide, 0, pane_id, WIDE_SIZE);
        state.g6_enable_pane_mouse_tracking(wide, 0, pane_id)
    };

    let selected = handler
        .handle(Request::SelectWindow(SelectWindowRequest {
            target: WindowTarget::with_window(narrow.clone(), FREE_INDEX),
        }))
        .await;
    assert!(
        matches!(selected, Response::SelectWindow(_)),
        "{selected:?}"
    );

    let (wide_tx, wide_rx) = mpsc::unbounded_channel();
    let (narrow_tx, narrow_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(MOUSE_WIDE_PID, wide.clone(), wide_tx)
        .await;
    handler
        .register_attach(MOUSE_NARROW_PID, narrow.clone(), narrow_tx)
        .await;
    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(&PaneTarget::with_window(wide.clone(), 0, 0));
        state.start_pane_input_capture_for_test(&PaneTarget::with_window(
            narrow.clone(),
            FREE_INDEX,
            0,
        ));
    }
    (handler, modes, wide_rx, narrow_rx)
}

/// Establish the pane origin, the effective rectangles and the clip both origins impose, so the
/// coordinate expectations below rest on a verified geometry rather than an assumed one.
async fn assert_rectangles_and_clip(
    handler: &RequestHandler,
    wide: &SessionName,
    narrow: &SessionName,
) {
    let state = handler.state.lock().await;
    let through_wide = crate::mouse::pane_mouse_coordinates_for_target(
        &state,
        &PaneTarget::with_window(wide.clone(), 0, 0),
    )
    .expect("the source winlink resolves its own geometry");
    let through_narrow = crate::mouse::pane_mouse_coordinates_for_target(
        &state,
        &PaneTarget::with_window(narrow.clone(), FREE_INDEX, 0),
    )
    .expect("the linked winlink resolves its own geometry");

    assert_eq!(
        through_wide.relative_position(4, 1),
        Some((4, 1)),
        "the pane sits at the source window origin, so a client cell maps to itself"
    );
    assert_eq!(
        through_narrow.relative_position(4, 1),
        Some((4, 1)),
        "and at the linked window origin too"
    );
    assert_eq!(
        through_wide.relative_position(29, 1),
        Some((29, 1)),
        "column 29 is inside the wide rectangle"
    );
    assert_eq!(
        through_narrow.relative_position(29, 1),
        None,
        "and outside the narrow one, which is 20 columns wide"
    );
    assert_eq!(
        through_wide.relative_position(4, 15),
        Some((4, 15)),
        "row 15 is inside the wide rectangle once the status line is accounted for"
    );
    assert_eq!(
        through_narrow.relative_position(4, 15),
        None,
        "and outside the narrow one, which has 10 rows"
    );
}

/// A-01, the mouse residue: a real SGR press and release through each attached identity, captured
/// before the PTY with that link's own pane-local coordinates.
///
/// The mode is established by parsing ordinary application output, not by writing bits. Each origin
/// gets its own ordinary scene so repeated presses at one cell can never be read as a double or
/// triple click, and each injection is a coherent press/release pair. Since the wrapper returns
/// `Ok(())` for a delivery and for an ignore alike, every case asserts exact bucket contents.
#[tokio::test]
async fn a_real_sgr_press_and_release_through_each_link_is_captured_with_that_link_local_coordinates(
) {
    /// Client SGR(5,2); at a pane whose origin is (0,0) with no top status line this is pane-local
    /// zero-based (4,1), which re-encodes to SGR(5,2).
    const PRESS: &[u8] = b"\x1b[<0;5;2M";
    const RELEASE: &[u8] = b"\x1b[<0;5;2m";
    const EXPECTED: &[u8] = b"\x1b[<0;5;2M\x1b[<0;5;2m";

    for through_wide in [true, false] {
        let wide = SessionName::new("g6r7-wide").expect("valid session name");
        let narrow = SessionName::new("g6r7-narrow").expect("valid session name");
        let (handler, (mode_before, mode_after), _wide_rx, _narrow_rx) =
            mouse_scene(&wide, &narrow).await;

        assert_eq!(
            mode_before,
            MODE_CURSOR | MODE_WRAP,
            "the ordinary screen starts with no mouse mode at all"
        );
        assert_eq!(
            mode_after,
            MODE_CURSOR | MODE_WRAP | MODE_MOUSE_STANDARD | MODE_MOUSE_SGR,
            "parsing DECSET 1000 then 1006 established standard tracking and SGR reporting"
        );
        assert_ne!(
            mode_after & ALL_MOUSE_MODES,
            0,
            "so encode_mouse_event will no longer refuse the event"
        );
        assert_rectangles_and_clip(&handler, &wide, &narrow).await;

        let wide_target = PaneTarget::with_window(wide.clone(), 0, 0);
        let narrow_target = PaneTarget::with_window(narrow.clone(), FREE_INDEX, 0);
        let (pid, injected, other) = if through_wide {
            (MOUSE_WIDE_PID, &wide_target, &narrow_target)
        } else {
            (MOUSE_NARROW_PID, &narrow_target, &wide_target)
        };

        for bytes in [PRESS, RELEASE] {
            handler
                .handle_attached_live_input_for_test(pid, bytes)
                .await
                .expect("each half of the press/release pair must succeed");
        }

        let state = handler.state.lock().await;
        assert_eq!(
            state.pane_input_capture_for_test(injected).as_deref(),
            Some(EXPECTED),
            "the injecting origin captured the report re-encoded to its own pane-local cell"
        );
        assert_eq!(
            state.pane_input_capture_for_test(other).as_deref(),
            Some(&[][..]),
            "and the other valid origin received nothing at all"
        );
    }
}

/// The column and row discriminants: a cell inside the wide rectangle and outside the narrow one is
/// delivered through the wide origin and ignored through the narrow one.
///
/// Both origins are valid and address the same pane, so this is the positive/ignored pair the review
/// asked for. The ignored case must still return the expected success while adding nothing to any
/// bucket, which is exactly what `Ok(())` alone cannot distinguish.
#[tokio::test]
async fn a_cell_beyond_the_narrow_link_is_delivered_by_the_wide_origin_and_ignored_by_the_narrow_one(
) {
    /// Column discriminant: client SGR(30,2), pane-local zero-based (29,1).
    const COLUMN: (&[u8], &[u8], &[u8]) = (
        b"\x1b[<0;30;2M",
        b"\x1b[<0;30;2m",
        b"\x1b[<0;30;2M\x1b[<0;30;2m",
    );
    /// Row discriminant: client SGR(5,16), pane-local zero-based (4,15).
    const ROW: (&[u8], &[u8], &[u8]) = (
        b"\x1b[<0;5;16M",
        b"\x1b[<0;5;16m",
        b"\x1b[<0;5;16M\x1b[<0;5;16m",
    );

    for (label, (press, release, expected)) in [("column", COLUMN), ("row", ROW)] {
        for through_wide in [true, false] {
            let wide = SessionName::new("g6r7d-wide").expect("valid session name");
            let narrow = SessionName::new("g6r7d-narrow").expect("valid session name");
            let (handler, (_, mode_after), _wide_rx, _narrow_rx) =
                mouse_scene(&wide, &narrow).await;
            assert_eq!(
                mode_after,
                MODE_CURSOR | MODE_WRAP | MODE_MOUSE_STANDARD | MODE_MOUSE_SGR,
                "{label}: mouse tracking established by parsing output"
            );
            assert_rectangles_and_clip(&handler, &wide, &narrow).await;

            let wide_target = PaneTarget::with_window(wide.clone(), 0, 0);
            let narrow_target = PaneTarget::with_window(narrow.clone(), FREE_INDEX, 0);
            let pid = if through_wide {
                MOUSE_WIDE_PID
            } else {
                MOUSE_NARROW_PID
            };
            for bytes in [press, release] {
                handler
                    .handle_attached_live_input_for_test(pid, bytes)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{label}: the injection must return the expected success: {error}")
                    });
            }

            let state = handler.state.lock().await;
            let wide_capture = state.pane_input_capture_for_test(&wide_target);
            let narrow_capture = state.pane_input_capture_for_test(&narrow_target);
            if through_wide {
                assert_eq!(
                    wide_capture.as_deref(),
                    Some(expected),
                    "{label}: the wide origin contains this cell, so it delivers the exact bytes"
                );
                assert_eq!(
                    narrow_capture.as_deref(),
                    Some(&[][..]),
                    "{label}: the narrow origin received nothing"
                );
            } else {
                assert_eq!(
                    narrow_capture.as_deref(),
                    Some(&[][..]),
                    "{label}: the cell is outside the narrow rectangle, so its own origin adds no \
                     bytes even though the call succeeded"
                );
                assert_eq!(
                    wide_capture.as_deref(),
                    Some(&[][..]),
                    "{label}: and nothing leaked into the other valid origin either"
                );
            }
        }
    }
}
