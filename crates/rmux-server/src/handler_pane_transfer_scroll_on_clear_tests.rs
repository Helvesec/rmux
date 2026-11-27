//! Dedicated SOC1 regressions for `scroll-on-clear` across pane transfers.
//!
//! `swap-pane`, `join-pane`, `move-pane` and `break-pane` move a live pane
//! runtime and its transcript between windows instead of recreating them, so
//! the resolved `scroll-on-clear` bool cached on the transcript survives the
//! move. Without an explicit re-resolution on the success path of each entry,
//! a transferred pane keeps the policy of the window it came from until the
//! next option mutation or respawn.
//!
//! The discriminator throughout is the erase itself, never an internal getter:
//! seed marker rows, record `history_size`, append `ED 2`, record it again and
//! capture a range spanning history. "on" grows history by exactly the number
//! of used rows and keeps the markers reachable; "off" leaves history untouched
//! and drops the markers entirely.

use super::pane_group_transfer_tests::{create_session, pane_id, pane_ids};
use super::RequestHandler;
use rmux_core::PaneId;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    BreakPaneRequest, CapturePaneRequest, JoinPaneRequest, LinkWindowRequest, MovePaneRequest,
    NewWindowRequest, OptionName, PaneOptionSetRequest, PaneTarget, PaneTargetRef, Request,
    Response, ScopeSelector, SessionName, SetOptionByNameRequest, SetOptionMode, SetOptionRequest,
    SplitDirection, SplitWindowRequest, SplitWindowTarget, SwapPaneRequest, WindowTarget,
};

/// The fixture session geometry, matching `create_session`.
const FIXTURE_ROWS: u32 = 24;
/// How many rows the seed actually uses, and therefore how far an "on" erase
/// must grow history.
const SEEDED_ROWS: usize = 3;
/// How many lines the history fillers emit; comfortably more than the fixture
/// height so the leading ones scroll out of the viewport.
const HISTORY_FILLER_LINES: usize = 32;

// ---------------------------------------------------------------------------
// Request builders
// ---------------------------------------------------------------------------

/// A vertical join request, matching the shape the sibling transfer fixtures
/// use. The destination window is re-split, so a transferred pane is resized;
/// the seed re-blanks the viewport with clamping cursor addressing, so the
/// discriminator does not depend on the pane's post-transfer height.
fn join_request(source: PaneTarget, target: PaneTarget) -> JoinPaneRequest {
    JoinPaneRequest {
        source,
        target,
        direction: SplitDirection::Vertical,
        detached: true,
        before: false,
        full_size: false,
        size: None,
    }
}

fn move_request(source: PaneTarget, target: PaneTarget) -> MovePaneRequest {
    MovePaneRequest {
        source,
        target,
        direction: SplitDirection::Vertical,
        detached: true,
        before: false,
        full_size: false,
        size: None,
    }
}

fn capture_request(target: PaneTarget, start: Option<i64>, end: Option<i64>) -> CapturePaneRequest {
    CapturePaneRequest {
        target,
        start,
        end,
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        include_format: false,
        hyperlinks: false,
        line_numbers: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }
}

// ---------------------------------------------------------------------------
// Fixture construction
// ---------------------------------------------------------------------------

/// Pins the quiet stdin-discard default command so a spawned pane never writes
/// rows of its own into the discriminator.
async fn pin_quiet_default_command(handler: &RequestHandler) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: crate::test_shell::stdin_discard_command(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Creates a detached fixture session whose first pane has finished starting.
async fn soc_session(handler: &RequestHandler, name: &str) -> SessionName {
    pin_quiet_default_command(handler).await;
    let session_name = create_session(handler, name).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            0,
            0,
        ))
        .await;
    session_name
}

/// Splits one explicit pane and returns the settled new pane.
///
/// `pane_group_transfer_tests::split_session` splits whichever window the
/// session currently points at; these fixtures hold several windows at once, so
/// the split has to name its pane.
async fn soc_split(handler: &RequestHandler, target: &PaneTarget) -> PaneTarget {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(target.clone()),
            // Side-by-side keeps both panes at the full fixture height.
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let pane = match response {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&pane)
        .await;
    pane
}

async fn soc_window(handler: &RequestHandler, session_name: &SessionName, window_index: u32) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: None,
            detached: true,
            environment: None,
            command: None,
            start_directory: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
            process_command: None,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            session_name.clone(),
            window_index,
            0,
        ))
        .await;
}

/// Links window 0 of the session into slot 1 of the same session, so the two
/// slots are aliases of one window holding one set of live panes.
async fn soc_session_with_duplicate_window_alias(
    handler: &RequestHandler,
    name: &str,
) -> SessionName {
    let session_name = soc_session(handler, name).await;
    let _second = soc_split(
        handler,
        &PaneTarget::with_window(session_name.clone(), 0, 0),
    )
    .await;
    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(session_name.clone(), 0),
            target: WindowTarget::with_window(session_name.clone(), 1),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    session_name
}

// ---------------------------------------------------------------------------
// Option plumbing
// ---------------------------------------------------------------------------

async fn set_scroll_on_clear(
    handler: &RequestHandler,
    scope: OptionScopeSelector,
    value: Option<&str>,
    unset: bool,
) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope,
            name: "scroll-on-clear".to_owned(),
            value: value.map(str::to_owned),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

async fn set_window_scroll_on_clear(
    handler: &RequestHandler,
    session_name: &SessionName,
    window_index: u32,
    value: &str,
) {
    set_scroll_on_clear(
        handler,
        OptionScopeSelector::Window(WindowTarget::with_window(
            session_name.clone(),
            window_index,
        )),
        Some(value),
        false,
    )
    .await;
}

async fn set_pane_scroll_on_clear(handler: &RequestHandler, target: &PaneTarget, value: &str) {
    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: "scroll-on-clear".to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "{response:?}"
    );
}

/// `set -p -u`: drops the pane override so the pane falls back to its window.
async fn unset_pane_scroll_on_clear(handler: &RequestHandler, target: &PaneTarget) {
    set_scroll_on_clear(
        handler,
        OptionScopeSelector::Pane(target.clone()),
        None,
        true,
    )
    .await;
}

// ---------------------------------------------------------------------------
// Transcript observation
// ---------------------------------------------------------------------------

async fn transcript_of(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> crate::pane_transcript::SharedPaneTranscript {
    let state = handler.state.lock().await;
    state
        .transcript_handle(target)
        .expect("pane transcript must exist")
}

async fn append_bytes(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let transcript = transcript_of(handler, target).await;
    transcript.lock().append_bytes(bytes);
}

async fn history_size(handler: &RequestHandler, target: &PaneTarget) -> usize {
    let transcript = transcript_of(handler, target).await;
    let size = transcript.lock().history_size();
    size
}

async fn capture(
    handler: &RequestHandler,
    target: &PaneTarget,
    start: Option<i64>,
    end: Option<i64>,
) -> String {
    let response = handler
        .handle(Request::CapturePane(Box::new(capture_request(
            target.clone(),
            start,
            end,
        ))))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let stdout = response
        .command_output()
        .expect("capture-pane -p returns command output")
        .stdout()
        .to_vec();
    String::from_utf8(stdout).expect("capture output is utf8")
}

/// Captures a range that starts at the oldest history line.
async fn capture_including_history(handler: &RequestHandler, target: &PaneTarget) -> String {
    let size = history_size(handler, target).await;
    let start = i64::try_from(size).expect("history size fits in i64");
    capture(handler, target, Some(-start), None).await
}

async fn pane_id_at(handler: &RequestHandler, target: &PaneTarget) -> PaneId {
    let state = handler.state.lock().await;
    pane_id(
        &state,
        target.session_name(),
        target.window_index(),
        target.pane_index(),
    )
}

/// Writes enough marker lines to push the leading ones out of the viewport.
///
/// Line feeds scroll into history regardless of `scroll-on-clear`, so this
/// gives a transferred pane pre-existing scrollback whose survival does not
/// depend on the policy under test.
async fn fill_history(handler: &RequestHandler, target: &PaneTarget, marker: &str) {
    let mut bytes = String::new();
    for line in 0..HISTORY_FILLER_LINES {
        bytes.push_str(&format!("{marker}{line:02}\r\n"));
    }
    append_bytes(handler, target, bytes.as_bytes()).await;
}

/// Blanks every row with `EL`, then writes three marker rows at the top.
///
/// `EL` and absolute cursor addressing are used instead of `ED 2` because the
/// seed must not itself consult the policy under test. Addressing clamps, so
/// blanking `FIXTURE_ROWS` rows is safe on a pane that a split made shorter.
fn seed_bytes(marker: &str) -> Vec<u8> {
    let mut bytes = String::new();
    for row in 1..=FIXTURE_ROWS {
        bytes.push_str(&format!("\x1b[{row};1H\x1b[K"));
    }
    for (offset, suffix) in ["A", "B", "C"].iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{marker}{suffix}", offset + 1));
    }
    bytes.into_bytes()
}

#[derive(Debug)]
struct EraseObservation {
    history_delta: usize,
    visible: String,
    full: String,
}

/// Seeds three marker rows, issues `ED 2`, and reports what the erase did.
async fn erase_and_observe(
    handler: &RequestHandler,
    target: &PaneTarget,
    marker: &str,
) -> EraseObservation {
    append_bytes(handler, target, &seed_bytes(marker)).await;
    let seeded = capture(handler, target, None, None).await;
    assert!(
        seeded.contains(&format!("{marker}A")) && seeded.contains(&format!("{marker}C")),
        "seed must be visible before the erase: {seeded:?}"
    );
    let before = history_size(handler, target).await;
    append_bytes(handler, target, b"\x1b[2J").await;
    let after = history_size(handler, target).await;
    let visible = capture(handler, target, None, None).await;
    let full = capture_including_history(handler, target).await;
    EraseObservation {
        history_delta: after.saturating_sub(before),
        visible,
        full,
    }
}

fn assert_kept(observation: &EraseObservation, marker: &str, label: &str) {
    assert_eq!(
        observation.history_delta, SEEDED_ROWS,
        "{label}: the used rows must reach scrollback: {observation:?}"
    );
    assert!(
        !observation.visible.contains(marker),
        "{label}: the viewport must be blank after the erase: {observation:?}"
    );
    assert!(
        observation.full.contains(&format!("{marker}A"))
            && observation.full.contains(&format!("{marker}C")),
        "{label}: a capture spanning history must still hold the rows: {observation:?}"
    );
}

fn assert_discarded(observation: &EraseObservation, marker: &str, label: &str) {
    assert_eq!(
        observation.history_delta, 0,
        "{label}: history must not grow: {observation:?}"
    );
    assert!(
        !observation.visible.contains(marker),
        "{label}: the viewport must be blank after the erase: {observation:?}"
    );
    assert!(
        !observation.full.contains(marker),
        "{label}: a capture spanning history must not hold the rows: {observation:?}"
    );
}

/// Observes one erase and asserts it followed `expected_on`.
async fn assert_policy(
    handler: &RequestHandler,
    target: &PaneTarget,
    marker: &str,
    expected_on: bool,
    label: &str,
) {
    let observation = erase_and_observe(handler, target, marker).await;
    if expected_on {
        assert_kept(&observation, marker, label);
    } else {
        assert_discarded(&observation, marker, label);
    }
}

// ---------------------------------------------------------------------------
// swap-pane
// ---------------------------------------------------------------------------

#[tokio::test]
async fn swap_pane_re_resolves_scroll_on_clear_for_both_swapped_panes() {
    let handler = RequestHandler::new();
    let session = soc_session(&handler, "soc-transfer-swap").await;
    soc_window(&handler, &session, 1).await;
    set_window_scroll_on_clear(&handler, &session, 0, "off").await;
    set_window_scroll_on_clear(&handler, &session, 1, "on").await;

    let first_slot = PaneTarget::with_window(session.clone(), 0, 0);
    let second_slot = PaneTarget::with_window(session.clone(), 1, 0);
    assert_policy(
        &handler,
        &first_slot,
        "XA1",
        false,
        "swap: source window off",
    )
    .await;
    assert_policy(
        &handler,
        &second_slot,
        "XA2",
        true,
        "swap: target window on",
    )
    .await;
    let off_window_pane = pane_id_at(&handler, &first_slot).await;
    let on_window_pane = pane_id_at(&handler, &second_slot).await;

    let response = handler
        .handle(Request::SwapPane(SwapPaneRequest {
            source: first_slot.clone(),
            target: second_slot.clone(),
            direction: None,
            detached: true,
            preserve_zoom: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapPane(_)), "{response:?}");

    // The very same runtimes traded slots; neither was recreated.
    assert_eq!(pane_id_at(&handler, &first_slot).await, on_window_pane);
    assert_eq!(pane_id_at(&handler, &second_slot).await, off_window_pane);

    assert_policy(
        &handler,
        &second_slot,
        "XA3",
        true,
        "swap: the pane that moved into the on window",
    )
    .await;
    assert_policy(
        &handler,
        &first_slot,
        "XA4",
        false,
        "swap: the pane that moved into the off window",
    )
    .await;
}

// ---------------------------------------------------------------------------
// join-pane and move-pane
// ---------------------------------------------------------------------------

/// Walks one pane off -> on -> off through `join-pane` or `move-pane`.
///
/// The pane carries no override, so every hop must be decided by the window it
/// has just landed in.
async fn assert_transfer_re_resolves_in_both_directions(label: &str, use_move: bool) {
    let handler = RequestHandler::new();
    let session = soc_session(&handler, label).await;
    // The source window keeps a second pane so the transfer never destroys it.
    let travelling = soc_split(&handler, &PaneTarget::with_window(session.clone(), 0, 0)).await;
    soc_window(&handler, &session, 1).await;
    soc_window(&handler, &session, 2).await;
    set_window_scroll_on_clear(&handler, &session, 0, "off").await;
    set_window_scroll_on_clear(&handler, &session, 1, "on").await;
    set_window_scroll_on_clear(&handler, &session, 2, "off").await;

    assert_policy(
        &handler,
        &travelling,
        "XB1",
        false,
        &format!("{label}: before any hop"),
    )
    .await;
    let travelling_id = pane_id_at(&handler, &travelling).await;

    let on_window = transfer(
        &handler,
        use_move,
        travelling,
        PaneTarget::with_window(session.clone(), 1, 0),
    )
    .await;
    assert_eq!(
        pane_id_at(&handler, &on_window).await,
        travelling_id,
        "the moved pane must be the same runtime"
    );
    assert_policy(&handler, &on_window, "XB2", true, "off -> on hop").await;

    let off_window = transfer(
        &handler,
        use_move,
        on_window,
        PaneTarget::with_window(session.clone(), 2, 0),
    )
    .await;
    assert_eq!(
        pane_id_at(&handler, &off_window).await,
        travelling_id,
        "the moved pane must still be the same runtime"
    );
    assert_policy(&handler, &off_window, "XB3", false, "on -> off hop").await;
}

/// Drives one hop through the requested public entry and returns the pane's
/// new address.
async fn transfer(
    handler: &RequestHandler,
    use_move: bool,
    source: PaneTarget,
    target: PaneTarget,
) -> PaneTarget {
    if use_move {
        let response = handler
            .handle(Request::MovePane(move_request(source, target)))
            .await;
        let Response::MovePane(response) = response else {
            panic!("expected move-pane success, got {response:?}");
        };
        response.target
    } else {
        let response = handler
            .handle(Request::JoinPane(join_request(source, target)))
            .await;
        let Response::JoinPane(response) = response else {
            panic!("expected join-pane success, got {response:?}");
        };
        response.target
    }
}

#[tokio::test]
async fn join_pane_re_resolves_scroll_on_clear_in_both_directions() {
    assert_transfer_re_resolves_in_both_directions("soc-transfer-join", false).await;
}

#[tokio::test]
async fn move_pane_re_resolves_scroll_on_clear_in_both_directions() {
    assert_transfer_re_resolves_in_both_directions("soc-transfer-move", true).await;
}

// ---------------------------------------------------------------------------
// break-pane
// ---------------------------------------------------------------------------

/// Breaks a pane out into a brand-new window and checks it adopts the value the
/// new window resolves, which is the window-global one because the new slot
/// carries no override of its own.
async fn assert_break_pane_adopts_destination(label: &str, destination_on: bool) {
    let handler = RequestHandler::new();
    set_scroll_on_clear(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some(if destination_on { "on" } else { "off" }),
        false,
    )
    .await;
    let session = soc_session(&handler, label).await;
    let source = soc_split(&handler, &PaneTarget::with_window(session.clone(), 0, 0)).await;
    set_window_scroll_on_clear(
        &handler,
        &session,
        0,
        if destination_on { "off" } else { "on" },
    )
    .await;

    assert_policy(
        &handler,
        &source,
        "XC1",
        !destination_on,
        "break: source window value",
    )
    .await;
    let source_id = pane_id_at(&handler, &source).await;

    let response = handler
        .handle(Request::BreakPane(Box::new(BreakPaneRequest {
            source,
            target: None,
            name: None,
            detached: true,
            after: false,
            before: false,
            print_target: false,
            format: None,
        })))
        .await;
    let Response::BreakPane(response) = response else {
        panic!("expected break-pane success, got {response:?}");
    };
    let broken_out = response.target;
    assert_ne!(
        broken_out.window_index(),
        0,
        "break-pane must land the pane in a new window"
    );
    assert_eq!(
        pane_id_at(&handler, &broken_out).await,
        source_id,
        "break-pane must move the runtime, not recreate it"
    );

    assert_policy(
        &handler,
        &broken_out,
        "XC2",
        destination_on,
        "break: destination window value",
    )
    .await;
}

#[tokio::test]
async fn break_pane_adopts_an_on_destination_window() {
    assert_break_pane_adopts_destination("soc-transfer-break-on", true).await;
}

#[tokio::test]
async fn break_pane_adopts_an_off_destination_window() {
    assert_break_pane_adopts_destination("soc-transfer-break-off", false).await;
}

// ---------------------------------------------------------------------------
// Pane-level override
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transferred_pane_override_beats_the_destination_window_until_it_is_unset() {
    let handler = RequestHandler::new();
    let session = soc_session(&handler, "soc-transfer-override").await;
    let travelling = soc_split(&handler, &PaneTarget::with_window(session.clone(), 0, 0)).await;
    soc_window(&handler, &session, 1).await;
    set_window_scroll_on_clear(&handler, &session, 0, "off").await;
    set_window_scroll_on_clear(&handler, &session, 1, "off").await;

    set_pane_scroll_on_clear(&handler, &travelling, "on").await;
    assert_policy(
        &handler,
        &travelling,
        "XD1",
        true,
        "override beats the source window",
    )
    .await;

    let response = handler
        .handle(Request::JoinPane(join_request(
            travelling,
            PaneTarget::with_window(session.clone(), 1, 0),
        )))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected join-pane success, got {response:?}");
    };
    let moved = response.target;

    assert_policy(
        &handler,
        &moved,
        "XD2",
        true,
        "the override survives the transfer",
    )
    .await;
    // The destination's own pane has no override and must stay off, proving the
    // destination window really does say "off".
    let resident = PaneTarget::with_window(session.clone(), 1, 1 - moved.pane_index());
    assert_policy(
        &handler,
        &resident,
        "XD3",
        false,
        "the destination resident keeps the window value",
    )
    .await;

    unset_pane_scroll_on_clear(&handler, &moved).await;
    assert_policy(
        &handler,
        &moved,
        "XD4",
        false,
        "set -p -u falls back to the destination window",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Window aliases
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transfer_into_an_aliased_window_resolves_once_and_keeps_the_alias_intact() {
    let handler = RequestHandler::new();
    let session = soc_session_with_duplicate_window_alias(&handler, "soc-transfer-alias").await;
    // Window slots 0 and 1 are two winlinks onto one window. Window options are
    // stored per slot, so both slots are given the same value: the refresh
    // visits the shared panes once per alias, and this fixture must not depend
    // on which alias it happens to visit last.
    set_window_scroll_on_clear(&handler, &session, 0, "off").await;
    set_window_scroll_on_clear(&handler, &session, 1, "off").await;
    // A separate source window, kept alive by a second pane.
    soc_window(&handler, &session, 2).await;
    let travelling = soc_split(&handler, &PaneTarget::with_window(session.clone(), 2, 0)).await;
    set_window_scroll_on_clear(&handler, &session, 2, "on").await;

    assert_policy(
        &handler,
        &travelling,
        "XE1",
        true,
        "alias: source window on",
    )
    .await;
    let travelling_id = pane_id_at(&handler, &travelling).await;
    let alias_before = {
        let state = handler.state.lock().await;
        pane_ids(&state, &session, 0)
    };
    assert_eq!(
        alias_before,
        {
            let state = handler.state.lock().await;
            pane_ids(&state, &session, 1)
        },
        "the two slots must alias one window before the transfer"
    );

    let response = handler
        .handle(Request::JoinPane(join_request(
            travelling,
            PaneTarget::with_window(session.clone(), 0, 0),
        )))
        .await;
    let Response::JoinPane(response) = response else {
        panic!("expected join-pane success, got {response:?}");
    };
    let moved = response.target;
    assert_eq!(moved.window_index(), 0);
    assert_eq!(pane_id_at(&handler, &moved).await, travelling_id);

    let (alias_zero, alias_one) = {
        let state = handler.state.lock().await;
        (pane_ids(&state, &session, 0), pane_ids(&state, &session, 1))
    };
    assert_eq!(
        alias_zero, alias_one,
        "the refresh must not corrupt the alias: both slots still list one pane set"
    );
    assert_eq!(
        alias_zero.len(),
        alias_before.len() + 1,
        "the transferred pane joined the shared window exactly once"
    );
    assert!(alias_zero.contains(&travelling_id));

    assert_policy(
        &handler,
        &moved,
        "XE2",
        false,
        "alias: the moved pane adopts the aliased window value",
    )
    .await;
    // The same live pane reached through the other winlink behaves identically.
    let through_alias = PaneTarget::with_window(session.clone(), 1, moved.pane_index());
    assert_eq!(pane_id_at(&handler, &through_alias).await, travelling_id);
    assert_policy(
        &handler,
        &through_alias,
        "XE3",
        false,
        "alias: the same pane reached through the second winlink",
    )
    .await;
    // A pane that was already in the aliased window is untouched.
    let resident = PaneTarget::with_window(session.clone(), 0, 0);
    assert_ne!(pane_id_at(&handler, &resident).await, travelling_id);
    assert_policy(
        &handler,
        &resident,
        "XE4",
        false,
        "alias: the resident pane keeps the aliased window value",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_transfer_leaves_the_policy_and_the_history_untouched() {
    let handler = RequestHandler::new();
    let session = soc_session(&handler, "soc-transfer-refused").await;
    let stationary = soc_split(&handler, &PaneTarget::with_window(session.clone(), 0, 0)).await;
    soc_window(&handler, &session, 1).await;
    set_window_scroll_on_clear(&handler, &session, 0, "off").await;
    set_window_scroll_on_clear(&handler, &session, 1, "on").await;

    assert_policy(&handler, &stationary, "XF1", false, "refused: before").await;
    let stationary_id = pane_id_at(&handler, &stationary).await;
    fill_history(&handler, &stationary, "XF9").await;
    let history_before = history_size(&handler, &stationary).await;
    assert!(history_before > 0, "the filler must reach scrollback");

    // A destination pane index that does not exist: `join_pane` fails on `?`
    // before it can reach the refresh.
    let response = handler
        .handle(Request::JoinPane(join_request(
            stationary.clone(),
            PaneTarget::with_window(session.clone(), 1, 99),
        )))
        .await;
    assert!(matches!(response, Response::Error(_)), "{response:?}");

    assert_eq!(
        pane_id_at(&handler, &stationary).await,
        stationary_id,
        "a refused transfer must leave the pane where it was"
    );
    assert_eq!(
        history_size(&handler, &stationary).await,
        history_before,
        "a refused transfer must not touch scrollback"
    );
    assert!(
        capture_including_history(&handler, &stationary)
            .await
            .contains("XF900"),
        "the pre-existing scrollback must still be reachable"
    );
    assert_policy(
        &handler,
        &stationary,
        "XF2",
        false,
        "refused: the pane keeps its own window value",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Runtime preservation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_re_resolved_pane_is_the_moved_runtime_and_keeps_its_scrollback() {
    let handler = RequestHandler::new();
    let session = soc_session(&handler, "soc-transfer-runtime").await;
    let travelling = soc_split(&handler, &PaneTarget::with_window(session.clone(), 0, 0)).await;
    soc_window(&handler, &session, 1).await;
    set_window_scroll_on_clear(&handler, &session, 0, "on").await;
    set_window_scroll_on_clear(&handler, &session, 1, "off").await;

    assert_policy(
        &handler,
        &travelling,
        "XG1",
        true,
        "runtime: before the move",
    )
    .await;
    let travelling_id = pane_id_at(&handler, &travelling).await;
    fill_history(&handler, &travelling, "XG9").await;
    let history_before = history_size(&handler, &travelling).await;
    assert!(history_before > 0, "the filler must reach scrollback");

    let response = handler
        .handle(Request::MovePane(move_request(
            travelling,
            PaneTarget::with_window(session.clone(), 1, 0),
        )))
        .await;
    let Response::MovePane(response) = response else {
        panic!("expected move-pane success, got {response:?}");
    };
    let moved = response.target;
    assert_eq!(moved.window_index(), 1);

    // A fresh pane would carry a new id and an empty transcript; the witness
    // below is only meaningful because neither is true here.
    assert_eq!(
        pane_id_at(&handler, &moved).await,
        travelling_id,
        "the transfer must move the runtime, not replace it"
    );
    // Splitting the destination shortens the pane, so the resize pushes further
    // visible rows down into history. Scrollback may therefore grow, but the
    // lines written before the transfer must never be lost.
    assert!(
        history_size(&handler, &moved).await >= history_before,
        "the moved transcript keeps its scrollback"
    );
    assert!(
        {
            let full = capture_including_history(&handler, &moved).await;
            full.contains("XG900") && full.contains("XG931")
        },
        "every line written before the transfer must survive it"
    );

    assert_policy(
        &handler,
        &moved,
        "XG2",
        false,
        "runtime: the moved pane obeys the destination window",
    )
    .await;
}
