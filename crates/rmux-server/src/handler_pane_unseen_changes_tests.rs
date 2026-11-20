//! PUN1 — output that arrives while a pane is being consulted in a mode.
//!
//! Every expectation below is a literal measured on the pinned tmux 3.7b in one
//! 80x24 fixture (`pun1/native/oracle-pun1.json`, cells `p01`..
//! `p04`): each reading is the `#{pane_unseen_changes}|#{pane_mode}` pair that
//! measurement printed after the same action. The source behind them: any
//! non-empty input raises the flag while a mode remains
//! (`tmux-3.7b/input.c:1036-1038`), leaving the LAST mode clears it
//! (`window.c:1275-1277`), and the variable answers `0`/`1` for a pane and
//! nothing at all without one (`format.c:2179-2190`).

use super::RequestHandler;
use rmux_core::command_parser::CommandParser;
use rmux_proto::{
    CopyModeRequest, DisplayMessageRequest, NewSessionRequest, PaneTarget, Request, Response,
    SessionName, SplitDirection, SplitWindowExtRequest, SplitWindowRequest, SplitWindowTarget,
    Target, TerminalSize,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

pub(super) async fn new_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = session_name(name);
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    session
}

/// A session whose panes run the quiet stdin-discard child, settled.
///
/// These readings put a pane in a mode and then count what arrives. A live shell
/// that prints its banner or prompt during the mode raises the flag the test
/// expects to stay down, so every pane created after this call is quiet.
async fn new_quiet_session(handler: &RequestHandler, name: &str) -> SessionName {
    handler.pin_quiet_default_command_for_test().await;
    let session = new_session(handler, name).await;
    handler
        .wait_for_pane_startup_output_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    session
}

/// Splits a quiet session's pane and waits for the new pane's startup output.
async fn split_quiet(handler: &RequestHandler, target: &PaneTarget, created: &PaneTarget) {
    split(handler, target).await;
    handler.wait_for_pane_startup_output_for_test(created).await;
}

async fn new_manual_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session = new_session(handler, name).await;
    // A split -I pane has a real transcript/output generation, but no native
    // reader that can publish a shell prompt between the manual publications.
    let response = handler
        .handle(Request::SplitWindowExt(Box::new(SplitWindowExtRequest {
            target: SplitWindowTarget::Pane(PaneTarget::with_window(session.clone(), 0, 0)),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
            command: None,
            process_command: None,
            start_directory: None,
            keep_alive_on_exit: None,
            detached: true,
            size: None,
            preserve_zoom: false,
            full_size: false,
            stdin_payload: Some(Vec::new()),
        })))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)), "{response:?}");
    queue(handler, &format!("kill-pane -t {name}:0.0"))
        .await
        .expect("remove the uncontrolled producer; the empty pane becomes pane 0");
    session
}

pub(super) async fn queue(
    handler: &RequestHandler,
    command: &str,
) -> Result<rmux_proto::CommandOutput, rmux_proto::RmuxError> {
    handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new().parse(command).unwrap(),
        )
        .await
}

/// The oracle's own reading of a pane, through the real format.
pub(super) async fn reading(handler: &RequestHandler, target: &PaneTarget) -> String {
    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target.clone())),
            print: true,
            message: Some("#{pane_unseen_changes}|#{pane_mode}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(message) = response else {
        panic!("display-message answered {response:?}")
    };
    let output = message
        .command_output()
        .expect("display-message prints its answer");
    String::from_utf8(output.stdout().to_vec())
        .expect("the answer is utf-8")
        .trim_end_matches('\n')
        .to_owned()
}

/// The pane's own terminal and the sender that publishes into it.
async fn pane_io(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> (
    crate::pane_transcript::SharedPaneTranscript,
    crate::pane_io::PaneOutputSender,
) {
    let state = handler.state.lock().await;
    (
        state
            .transcript_handle(target)
            .expect("the pane has a transcript"),
        state
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("the pane has an output sender"),
    )
}

/// Publishes bytes the way this pane's own terminal delivers them.
pub(super) async fn output(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let (transcript, sender) = pane_io(handler, target).await;
    crate::pane_io::publish_pane_bytes_for_test(&transcript, &sender, bytes.to_vec());
}

pub(super) async fn copy_mode(handler: &RequestHandler, target: &PaneTarget, cancel: bool) {
    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: cancel,
            scrollbar_scroll: false,
            source: None,
            page_up: !cancel,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)), "{response:?}");
}

/// The pane's own capture, as `capture-pane -p` prints it.
pub(super) async fn capture(handler: &RequestHandler, target: &str) -> String {
    let output = queue(handler, &format!("capture-pane -p -t {target}"))
        .await
        .expect("the pane can be captured");
    String::from_utf8(output.stdout().to_vec()).expect("the capture is utf-8")
}

/// The identifier of a pane of the fixture window.
async fn pane_id(
    handler: &RequestHandler,
    session: &SessionName,
    index: usize,
) -> rmux_core::PaneId {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .and_then(|session| session.window_at(0))
        .and_then(|window| window.panes().get(index).map(rmux_core::Pane::id))
        .expect("the fixture pane exists")
}

pub(super) async fn split(handler: &RequestHandler, target: &PaneTarget) {
    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Pane(target.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
}

#[tokio::test]
async fn manual_pane_unseen_changes_fixture_has_no_independent_output_producer() {
    let handler = RequestHandler::new();
    let session = new_manual_session(&handler, "pun1-producer").await;
    let identifier = pane_id(&handler, &session, 0).await;
    // A live shell can publish startup bytes between any two awaited actions,
    // even when the test's own publication carries a rejected generation.
    // This is a structural synchronization invariant, with no timing window.
    assert!(
        handler.state.lock().await.pane_is_empty(identifier),
        "manual output fixtures must not race a native shell's output reader"
    );
}

#[tokio::test]
async fn output_during_a_consultation_is_unseen_until_the_mode_is_left() {
    // Cell `p01`, reading for reading: the journey on the pane one consults.
    let handler = RequestHandler::new();
    let session = new_manual_session(&handler, "pun1-journey").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);

    assert_eq!(reading(&handler, &pane).await, "0|");
    // The tool writes outside any mode: what arrives is seen as it arrives.
    output(&handler, &pane, b"PUN1-OUTSIDE\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "0|");

    // Consulting the scrollback changes nothing by itself.
    copy_mode(&handler, &pane, false).await;
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");

    // The tool writes while the consultation is up.
    output(&handler, &pane, b"PUN1-DURING\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");

    // Leaving the mode is what clears it, and a re-entry starts clean.
    copy_mode(&handler, &pane, true).await;
    assert_eq!(reading(&handler, &pane).await, "0|");
    copy_mode(&handler, &pane, false).await;
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");
    copy_mode(&handler, &pane, true).await;
    assert_eq!(reading(&handler, &pane).await, "0|");
}

#[tokio::test]
async fn an_invisible_sequence_counts_and_a_neighbour_is_untouched() {
    // Cell `p02`: on the pin, `ESC [ ? 2 5 l` written during the consultation
    // raised the flag while the pane's own capture stayed byte for byte what it
    // was. What counts is that bytes arrived, not that something changed on the
    // screen. Beside it, cell `p01`'s witness pane: it answers for itself.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "pun1-isolation").await;
    let first = PaneTarget::with_window(session.clone(), 0, 0);
    let second = PaneTarget::with_window(session.clone(), 0, 1);
    split_quiet(&handler, &first, &second).await;

    copy_mode(&handler, &first, false).await;
    let before = capture(&handler, "pun1-isolation:0.0").await;
    output(&handler, &first, b"\x1b[?25l").await;
    assert_eq!(reading(&handler, &first).await, "1|copy-mode");
    assert_eq!(
        capture(&handler, "pun1-isolation:0.0").await,
        before,
        "the sequence that raised the flag printed nothing at all"
    );
    // The pane nobody wrote to answers for itself, and its own output outside a
    // mode is seen as it arrives.
    assert_eq!(reading(&handler, &second).await, "0|");
    output(&handler, &second, b"PUN1-NEIGHBOUR\r\n").await;
    assert_eq!(reading(&handler, &second).await, "0|");

    copy_mode(&handler, &first, true).await;
    assert_eq!(reading(&handler, &first).await, "0|");
}

#[tokio::test]
async fn only_leaving_the_last_mode_clears_what_was_unseen() {
    // Cell `p03`: two modes on the pane. On the pin the consultation and the
    // tree are both on the pane's own stack, and leaving ONE of them left the
    // flag exactly where it was (`1|tree-mode`); only the departure that
    // emptied the stack cleared it. This crate stacks the other way round - the
    // tree keeps the consultation it was opened over - so the mode that gives
    // way here is the tree and the one that remains is the consultation; what
    // is measured is the same invariant.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "pun1-nested").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    copy_mode(&handler, &pane, false).await;
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");
    output(&handler, &pane, b"PUN1-NESTED\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");

    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens over the consultation");
    assert_eq!(reading(&handler, &pane).await, "1|tree-mode");

    // The tree goes, the consultation remains: a mode subsists, so nothing is
    // cleared.
    handler
        .dismiss_mode_tree(attach_pid)
        .await
        .expect("the tree is dismissed");
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");

    // Leaving the last mode is what clears it.
    copy_mode(&handler, &pane, true).await;
    assert_eq!(reading(&handler, &pane).await, "0|");
}

#[tokio::test]
async fn a_listing_without_a_pane_answers_nothing() {
    // Cell `p04`: a format the pin never completes with a pane answers `[]`,
    // while a listing whose format IS completed with the session's current pane
    // answers for that pane (`[0]`).
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "pun1-absent").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);

    queue(&handler, "set-buffer -b pun1 PUN1-BUFFER")
        .await
        .expect("a buffer to list");
    let listed = queue(&handler, "list-buffers -F '[#{pane_unseen_changes}]'")
        .await
        .expect("the buffers are listed");
    assert_eq!(
        String::from_utf8(listed.stdout().to_vec()).expect("the listing is utf-8"),
        "[]\n"
    );

    // And the same variable, in a listing that does reach a pane, still answers
    // for it once the consultation has left something unseen.
    let sessions = queue(&handler, "list-sessions -F '[#{pane_unseen_changes}]'")
        .await
        .expect("the sessions are listed");
    assert_eq!(
        String::from_utf8(sessions.stdout().to_vec()).expect("the listing is utf-8"),
        "[0]\n"
    );
    copy_mode(&handler, &pane, false).await;
    output(&handler, &pane, b"PUN1-ABSENT\r\n").await;
    let sessions = queue(&handler, "list-sessions -F '[#{pane_unseen_changes}]'")
        .await
        .expect("the sessions are listed");
    assert_eq!(
        String::from_utf8(sessions.stdout().to_vec()).expect("the listing is utf-8"),
        "[1]\n"
    );
    let buffers = queue(&handler, "list-buffers -F '[#{pane_unseen_changes}]'")
        .await
        .expect("the buffers are listed");
    assert_eq!(
        String::from_utf8(buffers.stdout().to_vec()).expect("the listing is utf-8"),
        "[]\n"
    );
    copy_mode(&handler, &pane, true).await;
}

#[tokio::test]
async fn bytes_a_rejected_generation_carries_mark_nothing() {
    // The same small boundary, on its other side: a publication whose
    // generation the pane no longer accepts changes nothing at all, and the
    // next accepted one marks. The flag is raised inside the accepted
    // publication, so a reader that survived a respawn cannot mark the pane its
    // bytes no longer belong to.
    let handler = RequestHandler::new();
    let session = new_manual_session(&handler, "pun1-generation").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let (transcript, sender) = pane_io(&handler, &pane).await;
    let identifier = pane_id(&handler, &session, 0).await;
    let effects: crate::pane_io::PaneAlertCallback =
        std::sync::Arc::new(crate::pane_io::PaneEffectSink::new(
            |_event| {},
            std::sync::Arc::new(crate::pane_rename_queue::PaneRenameQueue::new()),
        ));

    copy_mode(&handler, &pane, false).await;
    let stale = sender.current_generation();
    sender.set_generation(stale + 1);
    crate::pane_io::publish_pane_bytes_through_effects(
        &session,
        identifier,
        &transcript,
        &sender,
        Some(stale),
        &effects,
        b"PUN1-STALE\r\n".to_vec(),
    )
    .await;
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");

    crate::pane_io::publish_pane_bytes_through_effects(
        &session,
        identifier,
        &transcript,
        &sender,
        Some(stale + 1),
        &effects,
        b"PUN1-CURRENT\r\n".to_vec(),
    )
    .await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");
    copy_mode(&handler, &pane, true).await;
}

#[tokio::test]
async fn what_the_server_writes_itself_marks_nothing() {
    // The flag belongs to the pane's own terminal. What this server draws into
    // a transcript itself - a dead pane's status line, a diagnostic - is not
    // something the consultation has not seen, and goes through the common
    // append, which marks nothing.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "pun1-synthetic").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);

    copy_mode(&handler, &pane, false).await;
    let identifier = pane_id(&handler, &session, 0).await;
    handler
        .state
        .lock()
        .await
        .append_bytes_to_runtime_pane_transcript(&session, identifier, b"PUN1-STATUS\r\n")
        .expect("the server draws its own line");
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");

    {
        let (transcript, _sender) = pane_io(&handler, &pane).await;
        let mut transcript = transcript.lock();
        let _ = transcript.append_bytes_with_effects(b"PUN1-DIAGNOSTIC\r\n");
    }
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");

    // The tool's own bytes, at the same moment, do mark.
    output(&handler, &pane, b"PUN1-TOOL\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");
    copy_mode(&handler, &pane, true).await;
}

#[tokio::test]
async fn the_flag_never_outlives_the_mode_at_the_terminal_reset() {
    // `reset_terminal_state` is this crate's own point, and it drops the mode
    // outright instead of going through the revision the modes use. That is a
    // divergence this lot does not touch: in the source `send-keys -R` resets
    // the parser and the palette and leaves the mode stack exactly where it was
    // (`tmux-3.7b/cmd-send-keys.c:223-227`). What this lot owes that point is
    // consistency - a flag that says something is unseen must not survive the
    // mode that would have shown it - so the reset is made explicit there.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "pun1-reset").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);

    copy_mode(&handler, &pane, false).await;
    output(&handler, &pane, b"PUN1-BEFORE-RESET\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");

    queue(&handler, "send-keys -R -t pun1-reset:0.0")
        .await
        .expect("the pane's terminal state is reset");
    assert_eq!(reading(&handler, &pane).await, "0|");
}
