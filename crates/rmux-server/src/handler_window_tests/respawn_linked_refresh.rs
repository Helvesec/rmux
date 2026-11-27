use super::*;
use crate::pane_io::{PaneObservationItem, PaneOutputReceiver};
use rmux_core::events::OutputCursorItem;
use tokio::time::{timeout_at, Instant};

const RESPAWN_MARKER: &[u8] = b"respawn-linked-alias-live-output";
const STARTUP_OUTPUT: &[u8] = b"\x1b[?9001h\x1b[?1004h";

async fn receive_respawn_marker(
    receiver: &mut PaneOutputReceiver,
    marker_sequence: u64,
    deadline: Instant,
) {
    loop {
        // Check even when output is continuously ready: timeout_at may poll a
        // ready receive first. Both clients share this one bounded deadline.
        assert!(Instant::now() < deadline, "respawn marker deadline elapsed");
        let received = timeout_at(deadline, receiver.recv_observed())
            .await
            .expect("respawn marker deadline elapsed");
        // A receive that was pending can become ready after the deadline and
        // still win timeout_at's inner-first poll. Check before using its event.
        assert!(Instant::now() < deadline, "respawn marker deadline elapsed");
        let PaneObservationItem::Output(OutputCursorItem::Event(event)) = received else {
            panic!("respawn receiver lost output continuity: {received:?}");
        };
        assert!(
            event.sequence() <= marker_sequence,
            "respawn receiver skipped the marker sequence"
        );
        if event.sequence() == marker_sequence {
            assert_eq!(event.bytes(), RESPAWN_MARKER);
            return;
        }
        // Ordinary child payload may precede the injected marker. Lifecycle,
        // invalidation and lag are handled above, never discarded as payload.
    }
}

fn respawn_refresh_target(
    control: AttachControl,
    expected_session: &SessionName,
) -> crate::pane_io::AttachTarget {
    let AttachControl::Switch(target) = control else {
        panic!("expected linked alias switch refresh, got {control:?}");
    };
    let target = target.into_target();
    assert_eq!(&target.session_name, expected_session);
    *target
}

async fn receive_latest_respawn_refresh(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
    expected_session: &SessionName,
) -> crate::pane_io::AttachTarget {
    let control = timeout(Duration::from_secs(2), receiver.recv())
        .await
        .expect("linked alias must receive a respawn refresh")
        .expect("linked alias control channel remains open");
    let mut latest = respawn_refresh_target(control, expected_session);
    while let Ok(control) = receiver.try_recv() {
        latest = respawn_refresh_target(control, expected_session);
    }
    latest
}

#[tokio::test]
async fn respawn_window_refreshes_attached_link_aliases_after_active_pane_is_removed() {
    let handler = RequestHandler::new();
    let owner = session_name("respawn-refresh-owner");
    let alias = session_name("respawn-refresh-alias");
    let alias_peer = session_name("respawn-refresh-alias-peer");
    create_session(&handler, owner.as_str()).await;
    create_session(&handler, alias.as_str()).await;
    create_grouped_session(&handler, alias_peer.as_str(), &alias).await;

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(owner.clone()),
            direction: SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    let split_target = match split {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected owner split success, got {response:?}"),
    };
    let (retained_pane_id, removed_active_pane_id, previous_generation, previous_output_generation) = {
        let mut state = handler.state.lock().await;
        let mut owner_session = state.sessions.session_setup(&owner).expect("owner exists");
        owner_session
            .select_pane_in_window(0, split_target.pane_index())
            .expect("split pane becomes active");
        let window = owner_session.window_at(0).expect("owner window exists");
        let retained_pane_id = window.pane(0).expect("retained pane exists").id();
        let removed_active_pane_id = window.active_pane().expect("active split pane exists").id();
        (
            retained_pane_id,
            removed_active_pane_id,
            state
                .pane_lifecycle(retained_pane_id)
                .expect("retained lifecycle exists")
                .generation,
            state
                .pane_output_for_target(&owner, 0, 0)
                .expect("retained pane output exists")
                .current_generation(),
        )
    };
    assert_ne!(retained_pane_id, removed_active_pane_id);

    let linked = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(owner.clone(), 0),
            target: WindowTarget::with_window(alias.clone(), 0),
            after: false,
            before: false,
            kill_destination: true,
            detached: true,
        }))
        .await;
    assert!(matches!(linked, Response::LinkWindow(_)), "{linked:?}");
    {
        let state = handler.state.lock().await;
        for session_name in [&owner, &alias, &alias_peer] {
            let window = state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(0))
                .expect("linked family window exists");
            assert_eq!(window.panes().len(), 2);
            assert_eq!(
                window
                    .active_pane()
                    .expect("linked active pane exists")
                    .id(),
                removed_active_pane_id,
                "{session_name} must initially render the pane respawn-window removes"
            );
        }
    }

    let (alias_tx, mut alias_rx) = mpsc::unbounded_channel();
    let (peer_tx, mut peer_rx) = mpsc::unbounded_channel();
    handler.register_attach(51, alias.clone(), alias_tx).await;
    handler
        .register_attach(52, alias_peer.clone(), peer_tx)
        .await;
    tokio::join!(
        drain_attach_controls(&mut alias_rx),
        drain_attach_controls(&mut peer_rx),
    );

    let response = handler
        .handle(Request::RespawnWindow(Box::new(RespawnWindowRequest {
            target: WindowTarget::with_window(owner.clone(), 0),
            kill: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
        })))
        .await;
    assert!(
        matches!(response, Response::RespawnWindow(_)),
        "{response:?}"
    );

    let mut alias_target = receive_latest_respawn_refresh(&mut alias_rx, &alias).await;
    let mut peer_target = receive_latest_respawn_refresh(&mut peer_rx, &alias_peer).await;

    let (output, generation) = {
        let state = handler.state.lock().await;
        let lifecycle = state
            .pane_lifecycle(retained_pane_id)
            .expect("retained pane lifecycle exists");
        assert!(lifecycle.generation > previous_generation);
        let generation = lifecycle.output_sequence;
        assert!(generation > previous_output_generation);
        assert!(state.pane_lifecycle(removed_active_pane_id).is_none());
        for session_name in [&owner, &alias, &alias_peer] {
            let window = state
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(0))
                .expect("respawned linked family window exists");
            assert_eq!(window.panes().len(), 1);
            assert_eq!(
                window.active_pane().expect("respawned pane exists").id(),
                retained_pane_id,
                "{session_name} must select the retained respawned pane"
            );
            assert_eq!(
                state
                    .pane_output_for_target(session_name, 0, 0)
                    .expect("linked runtime output exists")
                    .current_generation(),
                generation,
                "{session_name} must resolve the respawned generation"
            );
        }
        (
            state
                .pane_output_for_target(&alias, 0, 0)
                .expect("respawned alias output exists")
                .clone(),
            generation,
        )
    };
    assert_eq!(
        output.send_for_generation(Some(previous_output_generation), RESPAWN_MARKER.to_vec()),
        None,
        "the old runtime must not publish the marker"
    );
    // A quiet command can still emit terminal setup bytes through ConPTY.
    // Queue legitimate preceding output deterministically so the first-event
    // assumption cannot return when native startup happens to be quiet.
    output
        .send_for_generation(Some(generation), STARTUP_OUTPUT.to_vec())
        .expect("startup output belongs to the respawned generation");
    let marker_sequence = output
        .send_for_generation(Some(generation), RESPAWN_MARKER.to_vec())
        .expect("marker belongs to the respawned generation");
    let deadline = Instant::now() + Duration::from_secs(2);
    for target in [&mut alias_target, &mut peer_target] {
        receive_respawn_marker(&mut target.pane_output, marker_sequence, deadline).await;
    }
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "linked alias must receive a respawn refresh")]
async fn missing_respawn_refresh_is_rejected() {
    let (_sender, mut receiver) = mpsc::unbounded_channel();
    receive_latest_respawn_refresh(&mut receiver, &session_name("missing-refresh")).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "respawn marker deadline elapsed")]
async fn wrong_runtime_without_marker_is_rejected() {
    let wrong_runtime = crate::pane_io::pane_output_channel();
    let mut receiver = wrong_runtime.subscribe();
    wrong_runtime.send(STARTUP_OUTPUT.to_vec());
    let refreshed_runtime = crate::pane_io::pane_output_channel();
    refreshed_runtime.send(STARTUP_OUTPUT.to_vec());
    let sequence = refreshed_runtime.send(RESPAWN_MARKER.to_vec());
    receive_respawn_marker(
        &mut receiver,
        sequence,
        Instant::now() + Duration::from_secs(2),
    )
    .await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "respawn marker deadline elapsed")]
async fn expired_deadline_rejects_even_a_queued_marker() {
    let output = crate::pane_io::pane_output_channel();
    let mut receiver = output.subscribe();
    let sequence = output.send(RESPAWN_MARKER.to_vec());
    receive_respawn_marker(&mut receiver, sequence, Instant::now()).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "respawn marker deadline elapsed")]
async fn marker_ready_after_pending_receive_deadline_is_rejected() {
    use std::future::Future;
    use std::task::{Context, Waker};

    let output = crate::pane_io::pane_output_channel();
    let mut receiver = output.subscribe();
    let deadline = Instant::now() + Duration::from_secs(2);
    let receiving = receive_respawn_marker(&mut receiver, 0, deadline);
    tokio::pin!(receiving);
    assert!(receiving
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_pending());
    // Keep this future unspawned so its next poll sees both ready output and
    // an expired timer, after its initial clock check has already passed.
    tokio::time::advance(Duration::from_millis(2001)).await;
    assert_eq!(Instant::now(), deadline + Duration::from_millis(1));
    assert_eq!(output.send(RESPAWN_MARKER.to_vec()), 0);
    receiving.await;
}

#[tokio::test(start_paused = true)]
async fn preceding_output_does_not_renew_the_marker_deadline() {
    let output = crate::pane_io::pane_output_channel();
    let mut receiver = output.subscribe();
    let deadline = Instant::now() + Duration::from_secs(2);
    let receiving = tokio::spawn(async move {
        receive_respawn_marker(&mut receiver, 3, deadline).await;
    });
    tokio::task::yield_now().await;
    for _ in 0..3 {
        tokio::time::advance(Duration::from_millis(600)).await;
        output.send(STARTUP_OUTPUT.to_vec());
        tokio::task::yield_now().await;
        assert!(!receiving.is_finished());
    }
    tokio::time::advance(Duration::from_millis(201)).await;
    let failure = receiving
        .await
        .expect_err("the missing marker must time out");
    assert!(failure.is_panic());
    assert_eq!(Instant::now(), deadline + Duration::from_millis(1));
}

#[tokio::test]
#[should_panic(expected = "assertion `left == right` failed")]
async fn marker_sequence_requires_exact_bytes() {
    let output = crate::pane_io::pane_output_channel();
    let mut receiver = output.subscribe();
    let sequence = output.send(b"wrong-marker".to_vec());
    receive_respawn_marker(
        &mut receiver,
        sequence,
        Instant::now() + Duration::from_secs(2),
    )
    .await;
}

#[tokio::test]
#[should_panic(expected = "Invalidated")]
async fn stale_generation_refresh_is_rejected_even_with_marker() {
    let output = crate::pane_io::pane_output_channel();
    output.set_generation(1);
    let mut receiver = output.subscribe();
    output.reset_generation(2);
    let sequence = output
        .send_for_generation(Some(2), RESPAWN_MARKER.to_vec())
        .expect("current generation marker");
    receive_respawn_marker(
        &mut receiver,
        sequence,
        Instant::now() + Duration::from_secs(2),
    )
    .await;
}

#[tokio::test]
#[should_panic(expected = "ProcessExited")]
async fn process_exit_before_marker_is_rejected() {
    let output = crate::pane_io::pane_output_channel();
    let mut receiver = output.subscribe();
    output.send(Vec::new());
    let sequence = output.send(RESPAWN_MARKER.to_vec());
    receive_respawn_marker(
        &mut receiver,
        sequence,
        Instant::now() + Duration::from_secs(2),
    )
    .await;
}

#[tokio::test]
#[should_panic(expected = "Gap")]
async fn output_gap_before_marker_is_rejected() {
    let output = crate::pane_io::pane_output_channel_with_limits(1, 128);
    let mut receiver = output.subscribe();
    output.send(STARTUP_OUTPUT.to_vec());
    let sequence = output.send(RESPAWN_MARKER.to_vec());
    receive_respawn_marker(
        &mut receiver,
        sequence,
        Instant::now() + Duration::from_secs(2),
    )
    .await;
}
