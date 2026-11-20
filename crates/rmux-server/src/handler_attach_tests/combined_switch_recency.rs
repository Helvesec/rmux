//! Combined M9 + M39: what one attached session switch does to the two
//! independent orders it now touches at once.
//!
//! Neither isolated chain could measure this. M9's `fcedd32f` removed the
//! pre-switch `resize_session_for_attach_client` call for a client that already
//! holds an attach registration, so `commit_attached_session_switch` owns the
//! whole move. That removed call site also carried a `session.touch_attached()`
//! — under M39 a *session recency* mint. The credit that survives is the one
//! inside the commit's own `mutate_session_and_resize_window_terminal` closure,
//! which predates both chains.
//!
//! Two consequences follow, and only a combined tree can show them:
//!
//! * a successful attached switch credits its destination once, at the commit,
//!   and never before it;
//! * a switch that fails credits nothing at all, because the only remaining
//!   credit sits inside the transaction that rolls back.
//!
//! The second is a deliberate behaviour change relative to both isolated
//! chains and is pinned here rather than discovered later.
//!
//! `SessionRecency` is an opaque process-local token and the allocator behind
//! it is a process-wide `static`, so parallel tests mint tokens too. Every
//! assertion below therefore compares one session against *its own* earlier
//! token, never against an absolute value or a global delta.

use super::*;

use super::switch_frame_geometry::{
    frame_geometry, linked_alias_sessions, pane_pty_size, register_declared_attach,
    set_window_size_policy, window_content_size, CLIENT_SIZE, SOURCE_WINDOW_INDEX, STATUS_OFF,
    TARGET_WINDOW_INDEX,
};
use rmux_core::SessionRecency;

/// The moving client's geometry. It registers first, so under `latest` it is
/// the older vote right up to the moment it switches.
const MOVER_SIZE: TerminalSize = TerminalSize {
    cols: 120,
    rows: 50,
};
/// The client already resident in the destination, registered second and
/// therefore the newest vote until the switch commits.
const RESIDENT_SIZE: TerminalSize = CLIENT_SIZE;
/// The geometry a sized `attach-session` asks for. It never wins anything here:
/// the stored registration's vote is what ranks.
const REQUESTED_SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };

// This module's own pid block. The delivery pause is keyed by attach pid in a
// process-wide static, so these must not collide with any sibling module's.
const MOVER_PID: u32 = 94_201;
const RESIDENT_PID: u32 = 94_202;

/// Reads one session's current position in the recency order.
async fn session_recency(handler: &RequestHandler, session: &SessionName) -> SessionRecency {
    handler
        .state
        .lock()
        .await
        .sessions
        .session(session)
        .expect("session exists")
        .recency()
}

/// A bystander session, created and therefore used after the fixture's own
/// sessions, so it starts out ranked ahead of them.
async fn create_witness_session(handler: &RequestHandler, session: &SessionName) {
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(CLIENT_SIZE),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
}

/// `alpha:0` linked into `beta:1`, `status off` on both, one `window-size`
/// across both aliases of the single shared window.
async fn linked_family(handler: &RequestHandler, policy: &str) -> (SessionName, SessionName) {
    let (alpha, beta) = linked_alias_sessions(handler, STATUS_OFF, STATUS_OFF).await;
    set_window_size_policy(handler, &alpha, SOURCE_WINDOW_INDEX, policy).await;
    set_window_size_policy(handler, &beta, TARGET_WINDOW_INDEX, policy).await;
    (alpha, beta)
}

/// A successful attached switch credits its destination, and credits nothing
/// else.
///
/// The credit has to be *fresh*: a witness session used immediately before the
/// command must end up ranked behind the destination, which is the outcome the
/// targetless reader consumes. The source is not credited at all — leaving a
/// session is not use of it.
///
/// One credit versus two is invisible in an opaque total order, so the count
/// itself is pinned by this test's partner,
/// [`a_failed_attached_switch_leaves_the_destination_recency_unchanged`]: the
/// only surviving credit lives inside the switch transaction, so a command that
/// rolls back credits nothing. The pre-`fcedd32f` shape carried a second
/// `session.touch_attached()` in the pre-switch
/// `resize_session_for_attach_client`, which ran *outside* that transaction and
/// therefore survives the rollback. Restoring it leaves this test green and
/// turns that one red — which is where the count is measured.
///
/// Round-counting at the size-selection seam deliberately is *not* used: the
/// post-commit family reconciliation legitimately selects again, so the number
/// of selections does not count credits.
#[tokio::test]
async fn attached_switch_credits_the_destination_recency_exactly_once() {
    let handler = RequestHandler::new();
    let (alpha, beta) = linked_family(&handler, "latest").await;

    let mut mover_rx = register_declared_attach(&handler, MOVER_PID, &alpha, MOVER_SIZE).await;
    // Registered second, so the resident owns the shared window until the
    // switch makes the mover the newest sizing authority.
    let _resident_rx = register_declared_attach(&handler, RESIDENT_PID, &beta, RESIDENT_SIZE).await;
    drain_attach_controls(&mut mover_rx);

    assert_eq!(
        window_content_size(&handler, &beta, TARGET_WINDOW_INDEX).await,
        RESIDENT_SIZE,
        "the resident client must own the shared window before the switch"
    );
    // A session used immediately before the command. The destination has to end
    // up ahead of it, which is what makes the credit demonstrably this command's
    // rather than something it already carried.
    let witness = session_name("combined-switch-witness");
    create_witness_session(&handler, &witness).await;
    let alpha_before = session_recency(&handler, &alpha).await;
    let beta_before = session_recency(&handler, &beta).await;
    let witness_before = session_recency(&handler, &witness).await;
    assert!(
        witness_before > beta_before,
        "the fixture must leave the witness ranked ahead of the destination"
    );

    let switched = handler
        .dispatch(
            MOVER_PID,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await;
    assert!(
        matches!(switched.response, Response::SwitchClient(_)),
        "the switch must succeed, got {:?}",
        switched.response
    );

    let beta_after = session_recency(&handler, &beta).await;
    assert!(
        beta_after > witness_before,
        "the commit must credit the destination, and the credit must be newer \
         than a session used just before the command"
    );
    assert_eq!(
        session_recency(&handler, &alpha).await,
        alpha_before,
        "the source keeps the order it had before the client left"
    );
    assert_eq!(
        session_recency(&handler, &witness).await,
        witness_before,
        "a switch credits its destination and nothing else"
    );

    // The M9 half of the same command still holds: the moving client is the
    // window's newest sizing authority, through the frame and both aliases.
    let framed = frame_geometry(recv_switch_target(&mut mover_rx, "combined switch frame").await);
    assert_eq!(framed, MOVER_SIZE, "the switch frame carries the mover");
    for (alias, window_index) in [(&beta, TARGET_WINDOW_INDEX), (&alpha, SOURCE_WINDOW_INDEX)] {
        assert_eq!(
            window_content_size(&handler, alias, window_index).await,
            MOVER_SIZE,
            "alias {alias}:{window_index} must settle on the mover's geometry"
        );
        assert_eq!(
            pane_pty_size(&handler, alias, window_index).await,
            MOVER_SIZE,
            "the PTY behind {alias}:{window_index} must agree with the model"
        );
    }
}

/// The three deterministic states in which a switch's delivery authority is
/// already gone before the transaction commits.
#[derive(Clone, Copy, Debug)]
enum LostDelivery {
    /// The exact generation is latched closing by its own `detach-client`.
    Closing,
    /// The attach-control receiver has been dropped.
    ClosedReceiver,
    /// The bounded attach-control backlog is saturated and never drained.
    FullBacklog,
}

/// A failed attached switch leaves the destination's recency exactly where it
/// found it.
///
/// This is the combined-only behaviour change. In isolation M39 credits the
/// destination from the pre-switch resize, which runs before any delivery
/// precheck and therefore survives the command's failure; with M9's `fcedd32f`
/// that call is gone and the sole credit sits inside the transaction, so a
/// command that fails leaves the recency order untouched — the same boundary
/// M9 already gives the geometry.
#[tokio::test]
async fn a_failed_attached_switch_leaves_the_destination_recency_unchanged() {
    for lost in [
        LostDelivery::Closing,
        LostDelivery::ClosedReceiver,
        LostDelivery::FullBacklog,
    ] {
        assert_failed_switch_credits_nothing(lost).await;
    }
}

async fn assert_failed_switch_credits_nothing(lost: LostDelivery) {
    let handler = RequestHandler::new();
    let (alpha, beta) = linked_family(&handler, "largest").await;

    let mut mover_rx = register_declared_attach(&handler, MOVER_PID, &alpha, MOVER_SIZE).await;
    drain_attach_controls(&mut mover_rx);
    let identity = handler.active_attach_identity_for_test(MOVER_PID).await;

    let alpha_before = session_recency(&handler, &alpha).await;
    let beta_before = session_recency(&handler, &beta).await;

    // The pause lands after the command has selected its size and released
    // every lock, which is exactly the window in which a client can detach,
    // lose its receiver, or stop draining.
    let pause = handler.install_attached_size_selection_pause();
    let sized_attach = super::super::with_expected_attach_and_session_identity(
        identity,
        alpha.clone(),
        identity.session_id(),
        handler.dispatch(
            MOVER_PID,
            Request::AttachSessionExt2(Box::new(AttachSessionExt2Request {
                target: Some(beta.clone()),
                target_spec: Some(beta.to_string()),
                detach_other_clients: false,
                kill_other_clients: false,
                read_only: false,
                skip_environment_update: false,
                flags: None,
                working_directory: None,
                client_terminal: rmux_proto::ClientTerminalContext::default(),
                client_size: Some(REQUESTED_SIZE),
            })),
        ),
    );
    let lose_the_delivery = async {
        tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
            .await
            .expect("the sized attach-session reaches its selection pause");
        lose_delivery(&handler, lost, identity.attach_id(), &mut mover_rx).await;
        pause.release.notify_one();
    };
    let (attached, ()) = tokio::join!(sized_attach, lose_the_delivery);

    assert!(
        matches!(attached.response, Response::Error(_)),
        "{lost:?}: the sized attach-session must fail, got {:?}",
        attached.response
    );
    assert_eq!(
        session_recency(&handler, &beta).await,
        beta_before,
        "{lost:?}: a switch that never committed must not credit its destination"
    );
    assert_eq!(
        session_recency(&handler, &alpha).await,
        alpha_before,
        "{lost:?}: a failed switch must not credit the session it tried to leave"
    );
}

/// Removes exactly one delivery precondition, then proves the registration is
/// still discoverable as the same generation. Without that proof a row could
/// pass because the identity itself vanished rather than because the switch
/// refused to credit.
async fn lose_delivery(
    handler: &RequestHandler,
    lost: LostDelivery,
    expected_attach_id: u64,
    control_rx: &mut mpsc::UnboundedReceiver<AttachControl>,
) {
    match lost {
        LostDelivery::Closing => {
            let response = handler
                .dispatch(MOVER_PID, Request::DetachClient(DetachClientRequest))
                .await
                .response;
            assert!(
                matches!(response, Response::DetachClient(_)),
                "detach-client must succeed, got {response:?}"
            );
        }
        LostDelivery::ClosedReceiver => control_rx.close(),
        LostDelivery::FullBacklog => {
            let mut active_attach = handler.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&MOVER_PID)
                .expect("the attached client is registered");
            // One real oversized control, charged through the production
            // sender. The receiver stays open and simply never drains it.
            let payload = vec![
                0_u8;
                (super::super::attach_support::ATTACH_CONTROL_BACKLOG_LIMIT - 1)
                    * AttachControl::BACKLOG_UNIT_BYTES
            ];
            active
                .control_tx
                .send(AttachControl::Write(payload))
                .expect("the last control that fits the budget is accepted");
        }
    }

    let active_attach = handler.active_attach.lock().await;
    let active = active_attach
        .by_pid
        .get(&MOVER_PID)
        .expect("every one of these states keeps the registration under its pid");
    assert_eq!(
        active.id, expected_attach_id,
        "{lost:?} must not replace the captured generation"
    );
}

/// A rename that lands between a switch's selection and its commit moves the
/// store key without ending a lifetime, and must leave both orders coherent.
///
/// M39 made post-publication attach credit name-agnostic
/// (`matches_active_lifetime`) precisely so a rename of the same lifetime keeps
/// its credit; M9 made the moving client the window's newest sizing authority.
/// The two orders are independent and this pins them independently: renaming
/// the source mints no recency of its own, the destination is still credited
/// once, and the mover still carries the sizing order it voted with.
#[tokio::test]
async fn rename_between_switch_selection_and_commit_preserves_both_orders() {
    let handler = RequestHandler::new();
    let (alpha, beta) = linked_family(&handler, "latest").await;
    let renamed = session_name("combined-renamed-source");

    let mut mover_rx = register_declared_attach(&handler, MOVER_PID, &alpha, MOVER_SIZE).await;
    let _resident_rx = register_declared_attach(&handler, RESIDENT_PID, &beta, RESIDENT_SIZE).await;
    drain_attach_controls(&mut mover_rx);

    let alpha_before = session_recency(&handler, &alpha).await;
    let beta_before = session_recency(&handler, &beta).await;

    let pause = handler.install_attached_size_selection_pause();
    let switching = handler.dispatch(
        MOVER_PID,
        Request::SwitchClient(SwitchClientRequest {
            target: beta.clone(),
        }),
    );
    let rename_the_source = async {
        tokio::time::timeout(ATTACH_LIFECYCLE_TIMEOUT, pause.reached.notified())
            .await
            .expect("the switch reaches its selection pause");
        let response = handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: alpha.clone(),
                new_name: renamed.clone(),
            }))
            .await;
        assert!(
            matches!(response, Response::RenameSession(_)),
            "renaming the source must succeed, got {response:?}"
        );
        // The rename refreshes the family it renamed and enqueues a frame of
        // its own, at the geometry the shared window still has. Drain it here,
        // while the command is still parked, so the frame examined below is the
        // switch's own rather than the independent mutation's.
        drain_attach_controls(&mut mover_rx);
        pause.release.notify_one();
    };
    let (switched, ()) = tokio::join!(switching, rename_the_source);

    assert!(
        matches!(switched.response, Response::SwitchClient(_)),
        "a rename of the source must not fail the switch, got {:?}",
        switched.response
    );

    // Session order: the destination is credited once, and the rename itself
    // mints nothing — the renamed session keeps the exact token it held.
    assert!(
        session_recency(&handler, &beta).await > beta_before,
        "the destination is still credited by the commit"
    );
    assert_eq!(
        session_recency(&handler, &renamed).await,
        alpha_before,
        "a rename moves a store key, it is not use of the session"
    );

    // Sizing order: independent of the above, the mover became the window's
    // newest authority and every alias of the shared window agrees.
    let framed = frame_geometry(recv_switch_target(&mut mover_rx, "renamed switch frame").await);
    assert_eq!(framed, MOVER_SIZE, "the switch frame carries the mover");
    for (alias, window_index) in [
        (&beta, TARGET_WINDOW_INDEX),
        (&renamed, SOURCE_WINDOW_INDEX),
    ] {
        assert_eq!(
            window_content_size(&handler, alias, window_index).await,
            MOVER_SIZE,
            "alias {alias}:{window_index} must follow the mover's sizing order"
        );
    }
}

/// #181 × M9: the `latest` switch rows stay green with a popup open.
///
/// #181 changed how a popup's rows are captured and drawn, and the popup frame
/// is emitted to the same attach-control channel the switch frame uses. The
/// switch must still make the moving client the newest sizing authority, and
/// the frame the client receives for the switch must still be the geometry the
/// window keeps.
#[tokio::test]
async fn switch_latest_recency_holds_with_a_popup_open() {
    let handler = RequestHandler::new();
    let (alpha, beta) = linked_family(&handler, "latest").await;

    let mut mover_rx = register_declared_attach(&handler, MOVER_PID, &alpha, MOVER_SIZE).await;
    let _resident_rx = register_declared_attach(&handler, RESIDENT_PID, &beta, RESIDENT_SIZE).await;
    drain_attach_controls(&mut mover_rx);

    open_popup(&handler, MOVER_PID).await;
    drain_attach_controls(&mut mover_rx);
    assert!(
        client_has_overlay(&handler, MOVER_PID).await,
        "the popup must still be open when the switch runs"
    );

    let beta_before = session_recency(&handler, &beta).await;
    let switched = handler
        .dispatch(
            MOVER_PID,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await;
    assert!(
        matches!(switched.response, Response::SwitchClient(_)),
        "an open popup must not fail the switch, got {:?}",
        switched.response
    );

    let framed = frame_geometry(recv_switch_target(&mut mover_rx, "popup switch frame").await);
    assert_eq!(
        framed, MOVER_SIZE,
        "an open popup must not change which client the switch frame is sized for"
    );
    for (alias, window_index) in [(&beta, TARGET_WINDOW_INDEX), (&alpha, SOURCE_WINDOW_INDEX)] {
        assert_eq!(
            window_content_size(&handler, alias, window_index).await,
            MOVER_SIZE,
            "alias {alias}:{window_index} must settle on the mover's geometry"
        );
        assert_eq!(
            pane_pty_size(&handler, alias, window_index).await,
            MOVER_SIZE,
            "the PTY behind {alias}:{window_index} must agree with the model"
        );
    }
    assert!(
        session_recency(&handler, &beta).await > beta_before,
        "the destination is credited once whether or not a popup is open"
    );
}

async fn open_popup(handler: &RequestHandler, requester_pid: u32) {
    let parsed = handler
        .parse_control_commands("display-popup -N -T Combined -w 30 -h 8 -x C -y C")
        .await
        .expect("display-popup parses");
    let result = handler
        .execute_parsed_commands_for_test(requester_pid, parsed)
        .await
        .expect("display-popup executes");
    assert!(
        result.stdout().is_empty(),
        "display-popup writes nothing to stdout"
    );
}

async fn client_has_overlay(handler: &RequestHandler, requester_pid: u32) -> bool {
    handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&requester_pid)
        .expect("the attached client is registered")
        .overlay
        .is_some()
}
