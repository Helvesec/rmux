//! CQUIT1 — `copy-mode -q` closes every mode of the pane it addresses.
//!
//! `cmd-copy-mode.c:69-71` answers `-q` with `window_pane_reset_mode_all`, which
//! pops while the pane still has a mode (`window.c:1293-1298`); the pop that
//! empties the stack clears PANE_UNSEENCHANGES and puts the pane's own screen
//! back (`window.c:1274-1277`). The unitary pop of `send-keys -X cancel` stays
//! unitary (`window-copy.c:3519-3520`).
//!
//! The readings below are the pair `#{pane_unseen_changes}|#{pane_mode}` and are
//! the ones one bounded fixture measured on the pinned tmux 3.7b for this cell
//! (`cquit1/native/oracle-cquit1b.json`): a consultation over a held
//! tool answers `0|clock-mode`, what it masks answers `1|clock-mode`, and one
//! `-q` answers `0|` at once, before any key.

use super::pane_unseen_changes_tests::{
    capture, copy_mode, new_session, output, queue, reading, split,
};
use super::RequestHandler;

#[path = "handler_cquit_tests/fixture.rs"]
mod fixture;
#[path = "handler_cquit_tests/probe.rs"]
mod probe;
#[path = "handler_cquit_tests/trace.rs"]
mod trace;

use fixture::{new_quiet_session, split_quiet};
use rmux_proto::{
    ClockModeRequest, LinkWindowRequest, OptionName, PaneTarget, Request, Response, ScopeSelector,
    SetOptionMode, SetOptionRequest, TerminalSize, WindowTarget,
};
use tokio::time::{timeout, Duration};

/// The pane's own mode state, read where it really lives.
///
/// The response of the command is not the measurement: what answers here is the
/// transcript the pane keeps, so a command that answers `active: false` while a
/// mode is still installed cannot pass.
async fn modes_left(handler: &RequestHandler, target: &PaneTarget) -> (bool, Option<u64>) {
    let state = handler.state.lock().await;
    let transcript = state
        .transcript_handle(target)
        .expect("the pane has a transcript");
    let transcript = transcript.lock();
    (
        transcript.pane_in_mode(),
        transcript.clock_mode_generation(),
    )
}

/// What the attached client still holds of a tree: its state, its frame, the
/// identifier every persistent overlay is stamped with, and the epoch the
/// renderer compares against.
async fn client_tree(handler: &RequestHandler, attach_pid: u32) -> (bool, bool, u64, u64) {
    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&attach_pid)
        .expect("the registered client is still there");
    (
        active.mode_tree.is_some(),
        active.mode_tree_frame.is_some(),
        active.mode_tree_state_id,
        active
            .persistent_overlay_epoch
            .load(std::sync::atomic::Ordering::SeqCst),
    )
}

#[tokio::test]
async fn one_q_leaves_the_consultation_that_masked_the_tool() {
    // Cell `c01` of the native fixture, by request: a clock over the pane's own
    // tool, output arriving while it is masked, then one `-q`.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-clock").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    output(&handler, &pane, b"CQUIT-OUTSIDE\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "0|");

    queue(&handler, "clock-mode -t cquit-clock:0.0")
        .await
        .expect("the consultation opens over the tool");
    assert_eq!(reading(&handler, &pane).await, "0|clock-mode");
    output(&handler, &pane, b"CQUIT-DURING\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|clock-mode");

    // The command under measurement, and its reading before any key.
    copy_mode(&handler, &pane, true).await;
    assert_eq!(reading(&handler, &pane).await, "0|");

    // Not the answer of the command: the pane itself holds no mode any more and
    // the consultation's generation is gone with it, so its timer is stale.
    assert_eq!(modes_left(&handler, &pane).await, (false, None));

    // The same tool is back: its own screen carries what it wrote on both sides
    // of the consultation, and its next line is seen as it arrives.
    let screen = capture(&handler, "cquit-clock:0.0").await;
    assert!(screen.contains("CQUIT-OUTSIDE"), "{screen:?}");
    assert!(screen.contains("CQUIT-DURING"), "{screen:?}");
    output(&handler, &pane, b"CQUIT-BACK\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "0|");
    assert!(
        capture(&handler, "cquit-clock:0.0")
            .await
            .contains("CQUIT-BACK"),
        "the first exchange after the consultation reaches the pane's own screen"
    );
}

#[tokio::test]
async fn one_q_leaves_the_tree_and_the_consultation_under_it() {
    // The stacked case: a tree opened over a consultation. `-q` addresses the
    // pane, so both go, and the client keeps nothing of the tree it was shown.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-nested").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    // The R6 trace, around the two steps the reading below is measured after.
    // It observes and prints; it asserts nothing, waits for nothing, and leaves
    // every expectation of this guard exactly where it was.
    trace::facts(&handler, &pane, "before-register-attach").await;
    let mut attach_trace = trace::observe(&handler, &pane, "register-attach").await;
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    attach_trace.drain("after-register-attach");
    trace::facts(&handler, &pane, "after-register-attach").await;
    drop(attach_trace);

    let mut entry_trace = trace::observe(&handler, &pane, "copy-mode-entry").await;
    trace::facts(&handler, &pane, "before-copy-mode-entry").await;
    copy_mode(&handler, &pane, false).await;
    entry_trace.drain("after-copy-mode-entry");
    trace::facts(&handler, &pane, "after-copy-mode-entry").await;
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");
    output(&handler, &pane, b"CQUIT-NESTED\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "1|copy-mode");

    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens over the consultation");
    assert_eq!(reading(&handler, &pane).await, "1|tree-mode");
    let (_, _, state_id_before, _) = client_tree(&handler, attach_pid).await;

    copy_mode(&handler, &pane, true).await;

    assert_eq!(reading(&handler, &pane).await, "0|");
    assert_eq!(modes_left(&handler, &pane).await, (false, None));

    // The tree's own client state goes with it: no state, no frame to replay, a
    // new identifier and the overlay epoch advanced to it.
    let (tree, frame, state_id, epoch) = client_tree(&handler, attach_pid).await;
    assert!(!tree, "the client holds no tree any more");
    assert!(!frame, "and nothing left to redraw of it");
    assert!(
        state_id > state_id_before,
        "the tree's identifier moved on: {state_id_before} then {state_id}"
    );
    assert_eq!(epoch, state_id, "the persistent overlay epoch followed it");

    output(&handler, &pane, b"CQUIT-BACK\r\n").await;
    assert_eq!(reading(&handler, &pane).await, "0|");
    assert!(capture(&handler, "cquit-nested:0.0")
        .await
        .contains("CQUIT-BACK"));
}

#[tokio::test]
async fn q_on_a_normal_pane_is_a_no_op_and_a_tree_hosted_elsewhere_is_left_alone() {
    // The guard, not a third counter-example: `-q` answers on a pane that has no
    // mode without touching anything, and it addresses ONE pane - a tree hosted
    // by the neighbour is not a mode of the pane being addressed.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-guard").await;
    let first = PaneTarget::with_window(session.clone(), 0, 0);
    split_quiet(&handler, &first, 1).await;
    let second = PaneTarget::with_window(session.clone(), 0, 1);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    // A pane that never entered a mode answers, twice, without changing.
    assert_eq!(reading(&handler, &first).await, "0|");
    copy_mode(&handler, &first, true).await;
    assert_eq!(reading(&handler, &first).await, "0|");
    assert_eq!(modes_left(&handler, &first).await, (false, None));
    copy_mode(&handler, &first, true).await;
    assert_eq!(reading(&handler, &first).await, "0|");

    // The tree is opened on the neighbour, which is the pane hosting it.
    queue(&handler, "select-pane -t cquit-guard:0.1")
        .await
        .expect("the neighbour becomes the current pane");
    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens on the neighbour");
    assert_eq!(reading(&handler, &second).await, "0|tree-mode");
    let (tree_before, _, state_id_before, _) = client_tree(&handler, attach_pid).await;
    assert!(tree_before);

    // `-q` on the pane that hosts nothing leaves the neighbour's tree alone.
    copy_mode(&handler, &first, true).await;
    assert_eq!(reading(&handler, &second).await, "0|tree-mode");
    assert_eq!(modes_left(&handler, &second).await, (true, None));
    let (tree_after, _, state_id_after, _) = client_tree(&handler, attach_pid).await;
    assert!(tree_after, "the client still holds the tree it was shown");
    assert_eq!(state_id_after, state_id_before);
}

// R2. `-q` walks the modes of ONE pane, and what it is allowed to pop next is
// only what the pop it just committed left underneath. Between two pops its
// effects are awaited, and during that window another consultation can enter or
// another pane can take the numeric slot: neither is this command's to remove.

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

async fn clock_mode(handler: &RequestHandler, target: &PaneTarget) {
    let response = handler
        .handle(Request::ClockMode(ClockModeRequest {
            target: Some(target.clone()),
        }))
        .await;
    assert!(matches!(response, Response::ClockMode(_)), "{response:?}");
}

/// The pane that holds a numeric slot right now, and the transcript it is on.
async fn slot_occupant(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> (
    rmux_core::PaneId,
    crate::pane_transcript::SharedPaneTranscript,
) {
    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()).map(rmux_core::Pane::id))
        .expect("the slot is occupied");
    let transcript = state
        .transcript_handle(target)
        .expect("the occupant has a transcript");
    (pane_id, transcript)
}

#[tokio::test]
async fn a_stale_q_keeps_a_clock_that_reentered_during_its_commit() {
    // The commit pause of the clock exit is the one the clock identity witness
    // already uses; here the stale caller is `-q` instead of a live key.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-reentry").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    clock_mode(&handler, &pane).await;
    assert_eq!(reading(&handler, &pane).await, "0|clock-mode");

    let pause = handler.install_clock_mode_exit_commit_pause_for_test(pane.clone());
    let q_handler = handler.clone();
    let q_pane = pane.clone();
    let q = tokio::spawn(async move { copy_mode(&q_handler, &q_pane, true).await });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("-q reaches the committed-effect pause of the consultation it left");

    // A new consultation opens while that -q is still inside its own effects.
    let reentry_handler = handler.clone();
    let reentry_pane = pane.clone();
    let reentry = tokio::spawn(async move { clock_mode(&reentry_handler, &reentry_pane).await });
    timeout(TEST_TIMEOUT, async {
        loop {
            if handler
                .target_is_in_clock_mode(&pane)
                .await
                .expect("the clock target resolves during the reentry")
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the new consultation is visible before the old -q publishes");

    pause.release();
    timeout(TEST_TIMEOUT, q)
        .await
        .expect("-q completes")
        .expect("the -q task joins");
    timeout(TEST_TIMEOUT, reentry)
        .await
        .expect("the reentry completes")
        .expect("the reentry task joins");

    // The consultation that entered after that commit is not this -q's to pop.
    assert_eq!(reading(&handler, &pane).await, "0|clock-mode");
    assert!(handler
        .target_is_in_clock_mode(&pane)
        .await
        .expect("the clock target still resolves"));
}

#[tokio::test]
async fn a_stale_q_leaves_a_pane_that_replaced_the_slot_alone() {
    // Same window between the commit and the effects: the pane `-q` addressed
    // goes away and its neighbour, which is consulting its own scrollback, takes
    // the numeric slot. The slot is the same; the pane is not.
    let handler = RequestHandler::new();
    // Before any pane, any PTY and any mode: a detached session of 80x24 whose
    // status line is already off is the size an 80x24 client attaches to, so the
    // late reconciliation the kill's own continuation runs
    // (handler_pane/removal.rs:177-188 then :217) finds the size it already has
    // and returns without mutating (resize_policy.rs:535-536). The debt this
    // witness used to carry came from its preparation, not from the command it
    // measures. The precedent is handler_pane_float_transfer_tests.rs:99-100.
    queue(&handler, "set-option -g status off")
        .await
        .expect("the status line is off before this fixture builds anything");
    let session = new_quiet_session(&handler, "cquit-replaced").await;
    let first = PaneTarget::with_window(session.clone(), 0, 0);
    split_quiet(&handler, &first, 1).await;
    let second = PaneTarget::with_window(session.clone(), 0, 1);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    copy_mode(&handler, &second, false).await;
    assert_eq!(reading(&handler, &second).await, "0|copy-mode");
    let (neighbour_pane_id, neighbour_transcript) = slot_occupant(&handler, &second).await;

    clock_mode(&handler, &first).await;
    assert_eq!(reading(&handler, &first).await, "0|clock-mode");
    let (addressed_pane_id, addressed_transcript) = slot_occupant(&handler, &first).await;

    let pause = handler.install_clock_mode_exit_commit_pause_for_test(first.clone());
    let q_handler = handler.clone();
    let q_pane = first.clone();
    let q = tokio::spawn(async move { copy_mode(&q_handler, &q_pane, true).await });
    timeout(TEST_TIMEOUT, pause.wait_until_reached())
        .await
        .expect("-q reaches the committed-effect pause");

    // The kill has to run beside the paused -q, not in front of it: its own
    // notification publishes after the one that -q prepared under the locks it
    // has already left, so awaiting it here would never reach the release. The
    // clock identity witness sequences its reentry the same way.
    let kill_handler = handler.clone();
    let kill = tokio::spawn(async move {
        queue(&kill_handler, "kill-pane -t cquit-replaced:0.0")
            .await
            .expect("the pane -q addressed goes away");
    });
    timeout(TEST_TIMEOUT, async {
        loop {
            if slot_occupant(&handler, &first).await.0 == neighbour_pane_id {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the neighbour holds the slot before the stale -q publishes");
    let (slot_pane_id, slot_transcript) = slot_occupant(&handler, &first).await;
    assert_eq!(
        slot_pane_id, neighbour_pane_id,
        "the neighbour took the slot"
    );
    assert_ne!(slot_pane_id, addressed_pane_id);
    assert!(!slot_transcript.same_instance(&addressed_transcript));
    assert!(slot_transcript.same_instance(&neighbour_transcript));

    // What the kill did to B, BEFORE the stale -q is released: the kill really
    // changes B's geometry - it takes the whole window - and that is a different
    // fact from anything the stale command does. So what this guard compares the
    // end against is this reading, taken here, and not a zero assumed in advance.
    let before_release = slot_snapshot(&handler, &first).await;

    pause.release();
    timeout(TEST_TIMEOUT, q)
        .await
        .expect("-q completes")
        .expect("the -q task joins");
    timeout(TEST_TIMEOUT, kill)
        .await
        .expect("the kill completes")
        .expect("the kill task joins");

    // The new occupant of that slot keeps the consultation it opened for itself,
    // and the stale command left every one of these exactly as it found them:
    // the reading, the modes, the geometry the pane really has, the calls its
    // endpoint was asked for and the revision its output was invalidated at.
    let after_release = slot_snapshot(&handler, &first).await;
    assert_eq!(after_release.mode, "copy-mode");
    assert_eq!(
        after_release, before_release,
        "the stale -q changed nothing of what the kill had already left"
    );
    assert_eq!(modes_left(&handler, &first).await, (true, None));
    let (still_pane_id, still_transcript) = slot_occupant(&handler, &first).await;
    assert_eq!(
        still_pane_id, neighbour_pane_id,
        "and it is still that pane"
    );
    assert!(still_transcript.same_instance(&neighbour_transcript));
}

#[tokio::test]
async fn q_through_one_alias_clears_that_host_and_leaves_the_other_tree() {
    // The real guard: one window linked into two sessions, one registered client
    // each, a tree hosted by A through the first and a tree hosted by B through
    // the second. `-q` on the alias of A must take A's tree and leave B's, with
    // the client that was shown it untouched.
    let handler = RequestHandler::new();
    // Three producers and no more: A and B of the shared window, and the window
    // the other session opens for itself. The alias this guard addresses opens
    // no pane of its own, and every marker is consumed before the link.
    let first_session = new_quiet_session(&handler, "cquit-alias-s").await;
    let pane_a = PaneTarget::with_window(first_session.clone(), 0, 0);
    split_quiet(&handler, &pane_a, 1).await;
    let pane_b = PaneTarget::with_window(first_session.clone(), 0, 1);
    let second_session = new_quiet_session(&handler, "cquit-alias-t").await;
    let link = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(first_session.clone(), 0),
            target: WindowTarget::with_window(second_session.clone(), 4),
            after: false,
            before: false,
            kill_destination: false,
            detached: true,
        }))
        .await;
    assert!(matches!(link, Response::LinkWindow(_)), "{link:?}");
    let alias_a = PaneTarget::with_window(second_session.clone(), 4, 0);
    let alias_b = PaneTarget::with_window(second_session.clone(), 4, 1);

    let first_pid = std::process::id();
    let second_pid = std::process::id().saturating_add(1);
    let (first_tx, _first_rx) = tokio::sync::mpsc::unbounded_channel();
    let (second_tx, _second_rx) = tokio::sync::mpsc::unbounded_channel();
    let _first_attach = handler
        .register_attach(first_pid, first_session.clone(), first_tx)
        .await;
    let _second_attach = handler
        .register_attach(second_pid, second_session.clone(), second_tx)
        .await;

    // A tree hosted by A, opened by the client of the first session.
    queue(&handler, "select-pane -t cquit-alias-s:0.0")
        .await
        .expect("A is the current pane of that window");
    open_tree(&handler, first_pid).await;
    assert_eq!(reading(&handler, &pane_a).await, "0|tree-mode");

    // A tree hosted by B, opened by the client of the other session. That client
    // has to be looking at the linked window for its current pane to be B.
    queue(&handler, "select-pane -t cquit-alias-s:0.1")
        .await
        .expect("B is the current pane of that window");
    queue(&handler, "select-window -t cquit-alias-t:4")
        .await
        .expect("the other session looks at the window it linked");
    open_tree(&handler, second_pid).await;
    assert_eq!(reading(&handler, &pane_b).await, "0|tree-mode");
    let (_, before_frame, before_state_id, before_epoch) = client_tree(&handler, second_pid).await;

    // What -q publishes is read from the real events, subscribed before it runs;
    // nothing here waits on anything -q would have to publish itself.
    let mut events = handler.subscribe_lifecycle_events();
    copy_mode(&handler, &alias_a, true).await;

    // The pane whose mode changed is the host the tree recorded - A in its own
    // session - not the alias this command happened to be addressed by. A hook
    // reads that provenance as `hook_session_name`.
    let mut mode_changed = Vec::new();
    loop {
        match events.try_recv() {
            Ok(event) => {
                if let rmux_core::LifecycleEvent::PaneModeChanged { target } = &event.event {
                    let session = event
                        .formats
                        .iter()
                        .find(|(name, _)| name == "hook_session_name")
                        .map(|(_, value)| value.clone());
                    mode_changed.push((target.clone(), session));
                }
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                panic!("the lifecycle events lagged during this witness: {skipped}")
            }
        }
    }
    assert_eq!(
        mode_changed,
        vec![(pane_a.clone(), Some("cquit-alias-s".to_owned()))],
        "the mode change is published for the host that held the tree"
    );

    // A, reached through the other session's alias, has no mode left and the
    // client that was shown its tree holds nothing of it.
    assert_eq!(reading(&handler, &alias_a).await, "0|");
    assert_eq!(reading(&handler, &pane_a).await, "0|");
    assert_eq!(modes_left(&handler, &pane_a).await, (false, None));
    let (first_tree, first_frame, _, _) = client_tree(&handler, first_pid).await;
    assert!(
        !first_tree,
        "the client of the addressed host holds no tree"
    );
    assert!(!first_frame);

    // B keeps its own tree, and so does the client that was shown it.
    assert_eq!(reading(&handler, &pane_b).await, "0|tree-mode");
    assert_eq!(reading(&handler, &alias_b).await, "0|tree-mode");
    assert_eq!(modes_left(&handler, &pane_b).await, (true, None));
    let (second_tree, second_frame, second_state_id, second_epoch) =
        client_tree(&handler, second_pid).await;
    assert!(
        second_tree,
        "the other client still holds the tree it was shown"
    );
    assert_eq!(second_frame, before_frame);
    assert_eq!(second_state_id, before_state_id);
    assert_eq!(second_epoch, before_epoch);
}

/// One `choose-tree` opened by an exact client, the way the queue delivers it.
async fn open_tree(handler: &RequestHandler, requester_pid: u32) {
    let parsed = rmux_core::command_parser::CommandParser::new()
        .parse_arguments(["choose-tree", "-w"])
        .expect("choose-tree parses");
    let command = RequestHandler::parse_mode_tree_queue_command(parsed.commands()[0].clone())
        .expect("the mode-tree command parses")
        .expect("the mode-tree command is recognised");
    handler
        .execute_queued_mode_tree(
            requester_pid,
            command,
            &crate::handler::scripting_support::QueueExecutionContext::without_caller_cwd(),
        )
        .await
        .expect("the tree opens for that client");
}

// R3. Between the moment `-q` decides what it is walking and the moment it pops,
// it awaits `active_attach`; a consultation entering right there mutates the
// transcript alone, because the entry prepares everything under `state` and lets
// that lock go before it writes. The pane it addressed is unchanged, but what it
// holds is no longer what `-q` was walking, and adopting it would erase someone
// else's consultation.

#[tokio::test]
async fn a_concurrent_consultation_is_not_adopted_by_a_q_that_found_a_tree() {
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-race").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens on that pane");
    assert_eq!(reading(&handler, &pane).await, "0|tree-mode");

    // The transcript is retained BEFORE the race: `state` is held by the paused
    // `-q` for part of it, so nothing here may go through the handler for what it
    // needs to observe.
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(&pane)
            .expect("the pane has a transcript")
    };

    // A real consultation, suspended once it has prepared everything under
    // `state` and released it, just before it writes the transcript.
    let entry_pause = handler.install_copy_mode_entry_commit_pause_for_test(pane.clone());
    let entry_handler = handler.clone();
    let entry_pane = pane.clone();
    let entry = tokio::spawn(async move { copy_mode(&entry_handler, &entry_pane, false).await });
    timeout(TEST_TIMEOUT, entry_pause.wait_until_reached())
        .await
        .expect("the consultation reaches the point where it would write");

    // A real `-q`, suspended after it takes `state` and before it awaits
    // `active_attach`.
    let quit_pause = handler.install_cancel_all_tree_pause_for_test(pane.clone());
    let quit_handler = handler.clone();
    let quit_pane = pane.clone();
    let quit = tokio::spawn(async move { copy_mode(&quit_handler, &quit_pane, true).await });
    timeout(TEST_TIMEOUT, quit_pause.wait_until_reached())
        .await
        .expect("-q reaches the point where it would take the clients");

    // The test holds what `-q` is about to ask for, so the consultation really
    // lands inside that window.
    let held = handler.active_attach.lock().await;
    quit_pause.release();
    entry_pause.release();
    timeout(TEST_TIMEOUT, async {
        loop {
            if transcript.lock().pane_mode_name() == Some("copy-mode") {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the consultation is installed before -q may take the clients");
    drop(held);

    timeout(TEST_TIMEOUT, entry)
        .await
        .expect("the consultation completes")
        .expect("the consultation task joins");
    timeout(TEST_TIMEOUT, quit)
        .await
        .expect("-q completes")
        .expect("the -q task joins");

    // The consultation that entered during that window is not this -q's to pop.
    assert_eq!(
        transcript.lock().pane_mode_name(),
        Some("copy-mode"),
        "the transcript still holds the consultation that entered"
    );
    assert_eq!(reading(&handler, &pane).await, "0|copy-mode");
    assert_eq!(modes_left(&handler, &pane).await, (true, None));
    // Nothing is asserted here about the client state of the tree that was
    // replaced: this -q popped nothing, so it published no cleanup either, and
    // what the entry left behind is not this command's to repair.
}

/// Everything this parcours compares across a stale command, read at once.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SlotSnapshot {
    reading: String,
    mode: String,
    modes_left: (bool, Option<u64>),
    geometry: String,
    native_resize_calls: u64,
    invalidation_revision: u64,
}

async fn slot_snapshot(handler: &RequestHandler, target: &PaneTarget) -> SlotSnapshot {
    let reading = reading(handler, target).await;
    let mode = reading
        .split_once('|')
        .map(|(_, mode)| mode.to_owned())
        .unwrap_or_else(|| reading.clone());
    let state = handler.state.lock().await;
    let geometry = state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| {
            let geometry = pane.geometry();
            format!(
                "{}x{}+{}+{}",
                geometry.cols(),
                geometry.rows(),
                geometry.x(),
                geometry.y()
            )
        })
        .expect("the pane is in this window");
    // The revision this pane's output was last invalidated at, taken at the same
    // atomic frontier the observers of this parcours use; the receiver it hands
    // back is dropped at once.
    let invalidation_revision = {
        let sender = state
            .pane_output_for_target(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("the pane has an output sender");
        let (boundary, (), _receiver) = sender.capture_with_observer(|| ());
        boundary.invalidation_revision
    };
    let native_resize_calls = state.pane_native_resize_calls_for_test(target);
    drop(state);
    SlotSnapshot {
        reading,
        mode,
        modes_left: modes_left(handler, target).await,
        geometry,
        native_resize_calls,
        invalidation_revision,
    }
}

/// The mode this pane is in, and nothing else.
///
/// The unseen flag is not a criterion of a resize: any non-empty chunk raises it
/// while a mode is up (`pane_transcript.rs:1113`, and tmux `input.c:1025-1039`
/// says the same), and on Windows a real resize is answered with a repaint. What
/// a mode transition owes its endpoint is measured by the width the producer
/// reports and by the calls that endpoint was really asked for.
async fn pane_mode(handler: &RequestHandler, target: &PaneTarget) -> String {
    let value = reading(handler, target).await;
    value
        .split_once('|')
        .map(|(_, mode)| mode.to_owned())
        .unwrap_or(value)
}

/// How many native resizes this pane's endpoint has really been asked for.
///
/// The size alone cannot answer it: a resize to the size the endpoint already
/// holds leaves that size exactly as it was. On Windows the two do not cost the
/// same - the console answers an identical resize with a full repaint, and
/// those bytes reach the pane as if its own tool had written them.
async fn native_resize_calls(handler: &RequestHandler, target: &PaneTarget) -> u64 {
    handler
        .state
        .lock()
        .await
        .pane_native_resize_calls_for_test(target)
}

/// The size this pane's endpoint really holds, asked of the endpoint itself.
async fn endpoint_size(handler: &RequestHandler, target: &PaneTarget) -> TerminalSize {
    handler
        .wait_for_pane_startup_to_finish_for_test(target)
        .await;
    let master = {
        let mut state = handler.state.lock().await;
        state
            .clone_pane_master_if_alive(
                target.session_name(),
                target.window_index(),
                target.pane_index(),
            )
            .expect("pane terminal is alive")
    };
    let size = master.size().expect("pane terminal size is available");
    TerminalSize {
        cols: size.cols,
        rows: size.rows,
    }
}

async fn set_window_option(
    handler: &RequestHandler,
    target: &PaneTarget,
    option: OptionName,
    value: &str,
) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

#[tokio::test]
async fn a_tree_over_a_normal_pane_asks_its_endpoint_for_nothing() {
    // The causal witness of this revision: opening a tree over a pane that was
    // in no mode cannot change that pane's size, so its endpoint must not be
    // asked for one. What that costs when it is asked was measured on Windows:
    // the console answers an identical resize with a full repaint, and the pane
    // then reports as unseen bytes nothing of its own wrote.
    let handler = RequestHandler::new();
    let session = new_quiet_session(&handler, "cquit-tree-normal").await;
    let first = PaneTarget::with_window(session.clone(), 0, 0);
    split_quiet(&handler, &first, 1).await;
    let second = PaneTarget::with_window(session.clone(), 0, 1);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;

    queue(&handler, "select-pane -t cquit-tree-normal:0.1")
        .await
        .expect("the neighbour becomes the current pane");
    let size_before = endpoint_size(&handler, &second).await;
    let calls_before = native_resize_calls(&handler, &second).await;
    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens on the neighbour");

    assert_eq!(
        native_resize_calls(&handler, &second).await,
        calls_before,
        "a tree over a pane that was in no mode changes no geometry, so it asks for no resize"
    );
    assert_eq!(endpoint_size(&handler, &second).await, size_before);
    assert_eq!(reading(&handler, &second).await, "0|tree-mode");
    let (tree_before, _, state_id_before, _) = client_tree(&handler, attach_pid).await;
    assert!(tree_before);

    // And the neighbour's `-q` still leaves that tree exactly as it was.
    copy_mode(&handler, &first, true).await;
    assert_eq!(reading(&handler, &second).await, "0|tree-mode");
    assert_eq!(modes_left(&handler, &second).await, (true, None));
    let (tree_after, _, state_id_after, _) = client_tree(&handler, attach_pid).await;
    assert!(tree_after, "the client still holds the tree it was shown");
    assert_eq!(state_id_after, state_id_before);
}

#[tokio::test]
async fn a_tree_over_a_modal_copy_pane_gives_the_reserve_back() {
    // The other side of the same question: a tree replaces a consultation, so a
    // pane that was reserving a modal scrollbar stops reserving it, and that is
    // a real size change the endpoint must be told about.
    let handler = RequestHandler::new();
    let (session, probed) = probe::ProbedPane::new_session(&handler, "cquit-tree-modal").await;
    let pane = PaneTarget::with_window(session.clone(), 0, 0);
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = tokio::sync::mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(attach_pid, session.clone(), control_tx)
        .await;
    set_window_option(&handler, &pane, OptionName::PaneScrollbars, "modal").await;

    // The width the pane's own producer really has, at each frontier this
    // witness compares: asked by a file and answered by a file, so no
    // measurement of it ever reaches the pane's terminal.
    let width_before_copy = probed.measure("before-copy").await;

    let unreserved = endpoint_size(&handler, &pane).await;
    copy_mode(&handler, &pane, false).await;
    let reserved = endpoint_size(&handler, &pane).await;
    let width_after_copy = probed.measure("after-copy").await;

    assert_eq!(
        reserved.cols + 1,
        unreserved.cols,
        "the consultation reserves the modal scrollbar: {unreserved:?} then {reserved:?}"
    );

    let calls_before = native_resize_calls(&handler, &pane).await;
    queue(&handler, "choose-tree")
        .await
        .expect("a tree opens over the consultation");
    let width_after_tree = probed.measure("after-tree").await;
    let calls_after_tree = native_resize_calls(&handler, &pane).await;

    // What this transition owes its endpoint, measured where it can be seen: the
    // mode it left the pane in, the width the pane's own producer reports, and
    // the calls that endpoint was really asked for. The unseen flag is not part
    // of it - a real resize is answered with a repaint on Windows, and any
    // non-empty chunk raises that flag while a mode is up.
    assert_eq!(pane_mode(&handler, &pane).await, "tree-mode");
    let width_of = |value: &serde_json::Value| {
        value["cols"]
            .as_i64()
            .expect("the producer answers a width")
    };
    assert_eq!(
        width_of(&width_after_copy) + 1,
        width_of(&width_before_copy),
        "the consultation reserves one column of the real console: \
         {width_before_copy} then {width_after_copy}"
    );
    assert_eq!(
        width_of(&width_after_tree),
        width_of(&width_before_copy),
        "and the tree gives that column back: {width_after_tree}"
    );
    assert!(
        calls_after_tree > calls_before,
        "that is a real change, so the endpoint is asked for it"
    );
    // Those widths came from one process, and it is still this pane's own.
    probed.verify_identity(&handler, &pane).await;
    probed.finish();
}
