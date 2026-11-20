use super::*;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::live_render::LivePaneRender;
use crate::pane_io::types::{pane_output_channel, AttachTarget, PaneOutputSender};
use rmux_core::PaneGeometry;
use rmux_proto::TerminalSize;
use std::collections::VecDeque;

fn deep_coalescible_target(
    output: &PaneOutputSender,
    marker: u8,
    render_len: usize,
) -> AttachTarget {
    let size = TerminalSize { cols: 10, rows: 4 };
    let session = crate::session_fixture::SessionFixture::new(
        rmux_proto::SessionName::new(format!("switch-{marker}")).expect("valid session name"),
        size,
    );
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let transcript = crate::pane_transcript::PaneTranscript::shared(64, size);
    transcript.lock().append_bytes(&[marker]);
    let live_pane = Some(
        LivePaneRender::new_from_transcript(
            transcript,
            session.view(),
            rmux_core::OptionStore::new(),
            pane,
        )
        .expect("test target has a live render snapshot"),
    );
    let (pane_output_start_sequence, pane_output) = output.subscribe_live_from_now();
    AttachTarget {
        session_name: rmux_proto::SessionName::new(format!("switch-{marker}"))
            .expect("valid session name"),
        pane_master: None,
        pane_output,
        pane_output_start_sequence,
        render_frame: vec![marker; render_len.max(1)],
        outer_terminal: OuterTerminal::resolve(
            &rmux_core::OptionStore::default(),
            crate::outer_terminal::OuterTerminalContext::default(),
        ),
        client_title: None,
        cursor_style: 0.into(),
        active_pane_geometry: PaneGeometry::new(0, 0, size.cols, size.rows),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane,
    }
}

fn popup_overlay(marker: u8, render_generation: u64, overlay_generation: u64) -> OverlayFrame {
    OverlayFrame::persistent(vec![marker; 32], render_generation, overlay_generation)
}

fn receive_materialized(
    receiver: &mut mpsc::UnboundedReceiver<AttachControl>,
    backlog: &AtomicUsize,
) -> AttachControl {
    let control = receiver.try_recv().expect("attach control is queued");
    release_attach_control_backlog(backlog, control.received_backlog_units());
    control.materialize_received()
}

#[test]
fn popup_repaint_backpressure_retains_only_the_latest_frame() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 4, Arc::clone(&closing));
    let second_producer = sender.clone();

    for generation in 0_u64..64 {
        let producer = if generation % 2 == 0 {
            &sender
        } else {
            &second_producer
        };
        producer
            .send_popup_overlay(
                7,
                OverlayFrame::persistent(
                    vec![generation as u8; AttachControl::BACKLOG_UNIT_BYTES],
                    11,
                    generation,
                ),
            )
            .expect("a replacement popup repaint must fit in the existing slot");
        assert!(
            backlog.load(Ordering::Acquire) <= 2,
            "replacement repaints must retain at most one weighted frame"
        );
    }

    assert!(!closing.load(Ordering::SeqCst));
    let control = receive_materialized(&mut receiver, &backlog);
    let AttachControl::Overlay(frame) = control else {
        panic!("expected a popup overlay");
    };
    assert_eq!(frame.overlay_generation, 63);
    assert_eq!(frame.frame[0], 63);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    drop(frame);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn popup_overlay_replacement_stops_at_reliable_control_boundaries() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        16,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("first repaint fits");
    sender
        .send_popup_overlay(7, popup_overlay(2, 3, 2))
        .expect("replacement repaint fits");
    sender
        .send_popup_overlay(7, OverlayFrame::persistent(Vec::new(), 3, 3))
        .expect("popup clear remains reliable");
    sender
        .send_popup_overlay(7, popup_overlay(4, 3, 4))
        .expect("post-clear repaint fits");
    sender
        .send_popup_overlay(7, popup_overlay(5, 3, 5))
        .expect("post-clear replacement fits");
    sender
        .send(AttachControl::AdvancePersistentOverlayState(9))
        .expect("state barrier fits");
    sender
        .send_popup_overlay(7, popup_overlay(6, 3, 6))
        .expect("post-barrier repaint fits");
    sender
        .send(AttachControl::Detach)
        .expect("explicit detach remains reliable");
    sender
        .send_popup_overlay(7, popup_overlay(7, 3, 7))
        .expect("post-detach queue order remains defined");

    let AttachControl::Overlay(first) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected first popup repaint");
    };
    assert_eq!(first.frame[0], 2);
    let AttachControl::Overlay(clear) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected popup clear");
    };
    assert!(clear.frame.is_empty());
    let AttachControl::Overlay(second) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected second popup repaint");
    };
    assert_eq!(second.frame[0], 5);
    assert!(matches!(
        receive_materialized(&mut receiver, &backlog),
        AttachControl::AdvancePersistentOverlayState(9)
    ));
    let AttachControl::Overlay(third) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected third popup repaint");
    };
    assert_eq!(third.frame[0], 6);
    assert!(matches!(
        receive_materialized(&mut receiver, &backlog),
        AttachControl::Detach
    ));
    let AttachControl::Overlay(fourth) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected fourth popup repaint");
    };
    assert_eq!(fourth.frame[0], 7);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 4);
    drop((first, second, third, fourth));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn sibling_overlay_controls_are_popup_replacement_boundaries() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        8,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("popup repaint fits");
    sender
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            vec![2],
            3,
            2,
            9,
        )))
        .expect("state-carrying overlay remains reliable");
    sender
        .send_popup_overlay(7, popup_overlay(3, 3, 3))
        .expect("second popup repaint fits");
    sender
        .send(AttachControl::Overlay(OverlayFrame::new(vec![4], 3, 4)))
        .expect("transient overlay remains reliable");
    sender
        .send_popup_overlay(7, popup_overlay(5, 3, 5))
        .expect("third popup repaint fits");

    for marker in [1, 2, 3, 4, 5] {
        let AttachControl::Overlay(frame) = receive_materialized(&mut receiver, &backlog) else {
            panic!("expected ordered overlay frame");
        };
        assert_eq!(frame.frame[0], marker);
    }
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn popup_and_switch_replacement_slots_are_mutual_ordering_boundaries() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        16,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();

    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("first repaint fits");
    sender
        .send_popup_overlay(7, popup_overlay(2, 3, 2))
        .expect("replacement repaint fits");
    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 3, 32,
        )))
        .expect("first switch fits");
    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 4, 32,
        )))
        .expect("replacement switch fits");
    sender
        .send_popup_overlay(7, popup_overlay(5, 3, 5))
        .expect("second repaint run fits");
    sender
        .send_popup_overlay(7, popup_overlay(6, 3, 6))
        .expect("second replacement repaint fits");

    let AttachControl::Overlay(first) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected first popup repaint");
    };
    assert_eq!(first.frame[0], 2);
    let AttachControl::Switch(target) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected switch boundary");
    };
    let (target, switch_count) = target.into_target_with_count();
    assert_eq!(target.render_frame[0], 4);
    assert_eq!(switch_count, 2);
    let AttachControl::Overlay(second) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected second popup repaint");
    };
    assert_eq!(second.frame[0], 6);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    drop((first, second));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn an_older_popup_generation_cannot_replace_a_newer_queued_repaint() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send_popup_overlay(7, popup_overlay(9, 3, 9))
        .expect("newer repaint fits");
    sender
        .send_popup_overlay(7, popup_overlay(7, 3, 7))
        .expect("older racing repaint is harmless");

    let AttachControl::Overlay(frame) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected popup repaint");
    };
    assert_eq!(frame.overlay_generation, 9);
    assert_eq!(frame.frame[0], 9);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(frame);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn popup_identity_and_render_generation_limit_replacement_scope() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("first popup fits");
    sender
        .send_popup_overlay(8, popup_overlay(2, 3, 2))
        .expect("replacement popup keeps its own queue position");
    sender
        .send_popup_overlay(8, popup_overlay(3, 4, 3))
        .expect("new base render keeps its own queue position");

    for marker in [1, 2, 3] {
        let AttachControl::Overlay(frame) = receive_materialized(&mut receiver, &backlog) else {
            panic!("expected ordered popup repaint");
        };
        assert_eq!(frame.frame[0], marker);
    }
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn try_receive_materializes_popup_overlay_and_retains_its_reservation() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );
    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("popup repaint fits");

    let control = crate::pane_io::control::try_recv_attach_control(&mut receiver, &backlog)
        .expect("popup repaint is received");
    assert!(matches!(control, AttachControl::Overlay(_)));
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(control);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn async_receive_materializes_popup_overlay_and_retains_its_reservation() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );
    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("popup repaint fits");

    let mut deferred = VecDeque::new();
    let control =
        crate::pane_io::control::recv_attach_control(&mut deferred, Some(&mut receiver), &backlog)
            .await
            .expect("popup repaint is received");
    assert!(matches!(control, AttachControl::Overlay(_)));
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(control);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn a_dequeued_popup_slot_retains_only_one_newer_repaint() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );
    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("first popup repaint fits");

    let AttachControl::CoalescedPopupOverlay(in_flight) = receiver
        .try_recv()
        .expect("first popup repaint is dequeued")
    else {
        panic!("expected sender-side popup marker");
    };
    let first = in_flight.take().expect("dequeued repaint materializes");
    assert_eq!(first.frame[0], 1);
    assert_eq!(backlog.load(Ordering::Acquire), 1);

    for generation in 2_u64..64 {
        sender
            .send_popup_overlay(7, popup_overlay(generation as u8, 3, generation))
            .expect("one follow-up repaint slot remains bounded");
    }
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    let AttachControl::Overlay(latest) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected latest follow-up repaint");
    };
    assert_eq!(latest.overlay_generation, 63);
    assert_eq!(latest.frame[0], 63);
    drop(in_flight);
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    drop(first);
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(latest);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn growing_popup_repaint_still_enforces_the_weighted_backlog_limit() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 2, Arc::clone(&closing));
    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("one-unit repaint fits");
    sender
        .send_popup_overlay(
            7,
            OverlayFrame::persistent(vec![2; AttachControl::BACKLOG_UNIT_BYTES], 3, 2),
        )
        .expect("two-unit replacement fills the limit");

    let error = sender
        .send_popup_overlay(
            7,
            OverlayFrame::persistent(vec![3; 2 * AttachControl::BACKLOG_UNIT_BYTES], 3, 3),
        )
        .expect_err("three-unit replacement exceeds the unchanged limit");
    assert!(error.is_full());
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(backlog.load(Ordering::Acquire), 3);

    let AttachControl::Overlay(retained) = receive_materialized(&mut receiver, &backlog) else {
        panic!("expected last fitting repaint");
    };
    assert_eq!(retained.frame[0], 2);
    assert!(matches!(
        receive_materialized(&mut receiver, &backlog),
        AttachControl::Detach
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    drop(retained);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn dropping_receiver_releases_queued_popup_overlay_reservation() {
    let (inner, receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );
    sender
        .send_popup_overlay(7, popup_overlay(1, 3, 1))
        .expect("popup repaint fits");
    assert_eq!(backlog.load(Ordering::Acquire), 1);

    drop(receiver);

    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_retains_only_latest_consecutive_deep_switch() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        64,
        Arc::new(AtomicBool::new(false)),
    );
    let second_producer = sender.clone();
    let output = pane_output_channel();

    for marker in 0_u8..64 {
        let producer = if marker % 2 == 0 {
            &sender
        } else {
            &second_producer
        };
        producer
            .send(AttachControl::switch(deep_coalescible_target(
                &output, marker, 32,
            )))
            .expect("replacement switch fits");
        assert_eq!(
            output.receiver_count_for_test(),
            1,
            "replacing marker {marker} must drop the previous deep target"
        );
    }

    assert_eq!(backlog.load(Ordering::Acquire), 1);
    let control = receiver.try_recv().expect("one coalesced switch is queued");
    assert_eq!(control.received_backlog_units(), 0);
    let AttachControl::Switch(target) = control else {
        panic!("expected a switch control");
    };
    let (target, switch_count) = target.into_target_with_count();
    assert_eq!(switch_count, 64, "render generations must remain aligned");
    assert_eq!(target.render_frame[0], 63);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    drop(target);
    assert_eq!(output.receiver_count_for_test(), 0);
}

#[test]
fn interleaved_controls_cannot_open_unbounded_deep_switch_slots() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 16, Arc::clone(&closing));
    let second_producer = sender.clone();
    let output = pane_output_channel();

    for marker in 0..AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8 {
        let producer = if marker % 2 == 0 {
            &sender
        } else {
            &second_producer
        };
        producer
            .send(AttachControl::switch(deep_coalescible_target(
                &output, marker, 8,
            )))
            .expect("bounded coalesced slot fits");
        producer
            .send(AttachControl::Refresh)
            .expect("interleaved ordering boundary fits");
    }
    assert_eq!(
        sender.pending_deep_switches.load(Ordering::Acquire),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES
    );

    let rejected_marker = AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8;
    let error = second_producer
        .send(AttachControl::switch(deep_coalescible_target(
            &output,
            rejected_marker,
            8,
        )))
        .expect_err("interleaved controls cannot bypass the deep target cap");

    assert!(error.is_full());
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(
        output.receiver_count_for_test(),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES
    );
    assert_eq!(
        backlog.load(Ordering::Acquire),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES * 2 + 1,
        "each switch/boundary pair and one terminal detach remain accounted"
    );

    for marker in 0..AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8 {
        let AttachControl::Switch(target) = receiver.try_recv().expect("ordered switch") else {
            panic!("expected an ordered switch");
        };
        assert_eq!(target.into_target().render_frame[0], marker);
        let boundary = receiver.try_recv().expect("ordered refresh boundary");
        assert!(matches!(&boundary, AttachControl::Refresh));
        release_attach_control_backlog(&backlog, boundary.received_backlog_units());
    }
    assert_eq!(sender.pending_deep_switches.load(Ordering::Acquire), 0);
    let detach = receiver.try_recv().expect("terminal detach sentinel");
    assert!(matches!(&detach, AttachControl::Detach));
    release_attach_control_backlog(&backlog, detach.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_closes_switch_slot_at_interleaved_control() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        8,
        Arc::new(AtomicBool::new(false)),
    );
    let second_producer = sender.clone();
    let output = pane_output_channel();

    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 1, 8,
        )))
        .expect("first switch fits");
    second_producer
        .send(AttachControl::Write(b"boundary".to_vec()))
        .expect("interleaved boundary fits");
    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 2, 8,
        )))
        .expect("second switch fits");

    let AttachControl::Switch(first) = receiver.try_recv().expect("first switch") else {
        panic!("expected the first switch");
    };
    assert_eq!(first.into_target().render_frame[0], 1);
    let boundary = receiver.try_recv().expect("boundary control");
    assert!(matches!(&boundary, AttachControl::Write(bytes) if bytes == b"boundary"));
    release_attach_control_backlog(&backlog, boundary.received_backlog_units());
    let AttachControl::Switch(second) = receiver.try_recv().expect("second switch") else {
        panic!("expected the second switch");
    };
    assert_eq!(second.into_target().render_frame[0], 2);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_does_not_coalesce_persistent_switches() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        8,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();
    for marker in [1_u8, 2] {
        let mut target = deep_coalescible_target(&output, marker, 8);
        target.persistent_overlay_state_id = Some(u64::from(marker));
        sender
            .send(AttachControl::switch(target))
            .expect("persistent switch fits");
    }

    for marker in [1_u8, 2] {
        let control = receiver
            .try_recv()
            .expect("persistent switch remains queued");
        release_attach_control_backlog(&backlog, control.received_backlog_units());
        let AttachControl::Switch(target) = control else {
            panic!("expected a persistent switch");
        };
        assert_eq!(target.into_target().render_frame[0], marker);
    }
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_bounds_non_coalescible_deep_switches() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 16, Arc::clone(&closing));
    let second_producer = sender.clone();
    let output = pane_output_channel();
    for marker in 0..AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8 {
        let mut target = deep_coalescible_target(&output, marker, 8);
        target.persistent_overlay_state_id = Some(u64::from(marker));
        let producer = if marker % 2 == 0 {
            &sender
        } else {
            &second_producer
        };
        producer
            .send(AttachControl::switch(target))
            .expect("bounded persistent switch fits");
    }
    assert_eq!(
        sender.pending_deep_switches.load(Ordering::Acquire),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES
    );

    let rejected_marker = AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8;
    let mut rejected = deep_coalescible_target(&output, rejected_marker, 8);
    rejected.persistent_overlay_state_id = Some(u64::from(rejected_marker));
    let error = second_producer
        .send(AttachControl::switch(rejected))
        .expect_err("deep non-coalescible retention is capped");

    assert!(error.is_full());
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(
        output.receiver_count_for_test(),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES,
        "the rejected deep target must be dropped immediately"
    );
    assert_eq!(
        backlog.load(Ordering::Acquire),
        AttachControlSender::MAX_PENDING_DEEP_SWITCHES + 1,
        "queued switches and one terminal detach remain accounted"
    );

    for marker in 0..AttachControlSender::MAX_PENDING_DEEP_SWITCHES as u8 {
        let control = receiver
            .try_recv()
            .expect("persistent switch remains ordered");
        release_attach_control_backlog(&backlog, control.received_backlog_units());
        let AttachControl::Switch(target) = control else {
            panic!("expected a persistent switch");
        };
        assert_eq!(target.into_target().render_frame[0], marker);
    }
    assert_eq!(sender.pending_deep_switches.load(Ordering::Acquire), 0);
    let detach = receiver.try_recv().expect("terminal detach sentinel");
    assert!(matches!(&detach, AttachControl::Detach));
    release_attach_control_backlog(&backlog, detach.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn dropping_attach_control_receiver_releases_coalesced_switch() {
    let (inner, receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        8,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();
    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 1, 8,
        )))
        .expect("switch fits");
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    assert_eq!(output.receiver_count_for_test(), 1);

    drop(receiver);

    assert_eq!(backlog.load(Ordering::Acquire), 0);
    assert_eq!(output.receiver_count_for_test(), 0);
    assert!(sender.is_closed());
}

#[test]
fn dropping_attach_control_receiver_releases_deep_switch_permit() {
    let (inner, receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        8,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();
    let mut target = deep_coalescible_target(&output, 1, 8);
    target.persistent_overlay_state_id = Some(1);
    sender
        .send(AttachControl::switch(target))
        .expect("persistent switch fits");
    assert_eq!(sender.pending_deep_switches.load(Ordering::Acquire), 1);
    assert_eq!(backlog.load(Ordering::Acquire), 1);

    drop(receiver);

    assert_eq!(sender.pending_deep_switches.load(Ordering::Acquire), 0);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    assert_eq!(output.receiver_count_for_test(), 0);
    assert!(sender.is_closed());
}

#[test]
fn growing_coalesced_switch_still_enforces_weighted_limit() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 1, Arc::clone(&closing));
    let output = pane_output_channel();
    sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output, 1, 8,
        )))
        .expect("small switch fits");

    let error = sender
        .send(AttachControl::switch(deep_coalescible_target(
            &output,
            2,
            AttachControl::BACKLOG_UNIT_BYTES,
        )))
        .expect_err("larger replacement exceeds the weighted limit");

    assert!(error.is_full());
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(
        output.receiver_count_for_test(),
        1,
        "the rejected replacement is dropped while the earlier queued target survives"
    );
    assert_eq!(
        backlog.load(Ordering::Acquire),
        2,
        "the original slot and one terminal detach sentinel remain accounted"
    );
    let AttachControl::Switch(target) = receiver.try_recv().expect("original switch") else {
        panic!("expected the original switch");
    };
    assert_eq!(target.into_target().render_frame[0], 1);
    let detach = receiver.try_recv().expect("terminal detach sentinel");
    assert!(matches!(&detach, AttachControl::Detach));
    release_attach_control_backlog(&backlog, detach.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_shares_one_weighted_budget_across_payload_types() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 3, Arc::clone(&closing));

    sender
        .send(AttachControl::Refresh)
        .expect("one small control fits");
    sender
        .send(AttachControl::Overlay(OverlayFrame::new(
            vec![b'x'; AttachControl::BACKLOG_UNIT_BYTES],
            0,
            0,
        )))
        .expect("two-unit overlay fills the shared budget");
    assert_eq!(backlog.load(Ordering::Acquire), 3);

    let received = receiver.try_recv().expect("receive first control");
    release_attach_control_backlog(&backlog, received.backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    sender
        .send(AttachControl::ClipboardWrite {
            bytes: vec![b'y'],
            reservation: None,
        })
        .expect("released control capacity is reusable");
    assert_eq!(backlog.load(Ordering::Acquire), 3);

    assert!(sender.send(AttachControl::Refresh).is_err());
    assert!(closing.load(Ordering::SeqCst));
    assert_eq!(
        backlog.load(Ordering::Acquire),
        4,
        "one accounted unit of terminal headroom is added exactly once"
    );
    assert!(sender.send(AttachControl::Refresh).is_err());
    assert_eq!(backlog.load(Ordering::Acquire), 4);

    while let Ok(control) = receiver.try_recv() {
        release_attach_control_backlog(&backlog, control.received_backlog_units());
        drop(control);
    }
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn attach_control_sender_rolls_back_reservation_when_receiver_closed() {
    let (inner, receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let closing = Arc::new(AtomicBool::new(false));
    let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 4, Arc::clone(&closing));
    sender
        .send(AttachControl::Refresh)
        .expect("queued control reserves one unit");
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    drop(receiver);

    assert!(sender
        .send(AttachControl::Write(vec![
            b'x';
            AttachControl::BACKLOG_UNIT_BYTES
        ]))
        .is_err());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    assert!(closing.load(Ordering::SeqCst));
}

#[test]
fn payload_control_cannot_release_a_later_control_reservation() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send(AttachControl::Write(b"payload".to_vec()))
        .expect("payload control fits");
    sender
        .send(AttachControl::Refresh)
        .expect("refresh control fits");
    assert_eq!(backlog.load(Ordering::Acquire), 2);

    let payload = receiver.try_recv().expect("payload arrives first");
    release_attach_control_backlog(&backlog, payload.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 1);
    let refresh = receiver.try_recv().expect("refresh remains queued");
    assert!(matches!(&refresh, AttachControl::Refresh));
    release_attach_control_backlog(&backlog, refresh.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn receiver_deferred_clipboard_cannot_release_a_later_control_reservation() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );

    sender
        .send(AttachControl::ClipboardWrite {
            bytes: vec![b'x'],
            reservation: None,
        })
        .expect("clipboard control fits");
    sender
        .send(AttachControl::Refresh)
        .expect("later refresh control fits");
    assert_eq!(backlog.load(Ordering::Acquire), 2);

    let clipboard = receiver.try_recv().expect("clipboard control arrives");
    assert_eq!(clipboard.received_backlog_units(), 0);
    release_attach_control_backlog(&backlog, clipboard.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 2);
    drop(clipboard);
    assert_eq!(
        backlog.load(Ordering::Acquire),
        1,
        "the later refresh keeps its own reservation"
    );
    let refresh = receiver.try_recv().expect("refresh remains queued");
    release_attach_control_backlog(&backlog, refresh.received_backlog_units());
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[test]
fn every_attach_control_variant_has_a_positive_backlog_cost() {
    let shell = AttachShellCommand::new("cmd".to_owned(), "sh".to_owned(), "/".to_owned());
    let controls = [
        AttachControl::Detach,
        AttachControl::Exited,
        AttachControl::DetachKill,
        AttachControl::DetachExecShellCommand(shell.clone()),
        AttachControl::InteractiveInput,
        AttachControl::Refresh,
        AttachControl::AdvancePersistentOverlayState(1),
        AttachControl::Overlay(OverlayFrame::new(Vec::new(), 0, 0)),
        AttachControl::Write(Vec::new()),
        AttachControl::ClipboardWrite {
            bytes: Vec::new(),
            reservation: None,
        },
        AttachControl::LockShellCommand(shell),
        AttachControl::Suspend,
    ];

    for control in controls {
        assert_eq!(control.backlog_units(), 1, "{control:?}");
    }

    let large = vec![b'x'; AttachControl::BACKLOG_UNIT_BYTES];
    let large_text = "x".repeat(AttachControl::BACKLOG_UNIT_BYTES);
    let weighted_payloads = [
        AttachControl::Overlay(OverlayFrame::new(large.clone(), 0, 0)),
        AttachControl::Write(large.clone()),
        AttachControl::ClipboardWrite {
            bytes: large,
            reservation: None,
        },
        AttachControl::DetachExecShellCommand(AttachShellCommand::new(
            large_text.clone(),
            String::new(),
            String::new(),
        )),
        AttachControl::LockShellCommand(AttachShellCommand::new(
            large_text,
            String::new(),
            String::new(),
        )),
    ];
    for control in weighted_payloads {
        assert_eq!(
            control.backlog_units(),
            2,
            "{:?}",
            std::mem::discriminant(&control)
        );
    }

    let popup_backlog = Arc::new(AtomicUsize::new(1));
    let popup_slot = Arc::new(CoalescedPopupOverlay::new(
        PopupOverlayKey::new(7, 3),
        popup_overlay(1, 3, 1),
        1,
        Arc::clone(&popup_backlog),
    ));
    let popup_control = AttachControl::CoalescedPopupOverlay(popup_slot);
    assert_eq!(popup_control.backlog_units(), 1);
    assert_eq!(popup_control.received_backlog_units(), 0);
    drop(popup_control);
    assert_eq!(popup_backlog.load(Ordering::Acquire), 0);

    let output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: rmux_proto::SessionName::new("backlog-cost").expect("valid session name"),
        pane_master: None,
        pane_output,
        pane_output_start_sequence,
        render_frame: vec![b'x'; AttachControl::BACKLOG_UNIT_BYTES],
        outer_terminal: OuterTerminal::resolve(
            &rmux_core::OptionStore::default(),
            crate::outer_terminal::OuterTerminalContext::default(),
        ),
        client_title: None,
        cursor_style: 0.into(),
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
    };
    assert_eq!(AttachControl::switch(target).backlog_units(), 2);
}

/// A plain render refresh with no title still replaces its predecessor in the
/// sender's coalescing slot: only the newest frame is worth drawing.
#[test]
fn a_titleless_switch_is_still_replaced_by_a_later_refresh() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        64,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();

    for marker in 0_u8..3 {
        let mut target = deep_coalescible_target(&output, marker, 32);
        target.live_pane = None;
        assert!(target.is_coalescible_render_refresh());
        sender
            .send(AttachControl::switch(target))
            .expect("refresh fits");
    }

    let AttachControl::Switch(queued) = receiver.try_recv().expect("one switch is queued") else {
        panic!("expected a switch control");
    };
    assert_eq!(queued.into_target_with_count().0.render_frame[0], 2);
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));
}

/// A frame carrying OSC 0 must survive the sender's coalescing slot. The render
/// path commits the title to the client's remembered identity when it builds
/// the frame, and the next render deduplicates against that memory — so a
/// silently replaced frame leaves the outer terminal on the previous title with
/// nothing left to correct it (issue #182).
///
/// This runs at the queue layer on purpose: every render-level test still sees
/// the title in the frame it built, so only the queue can prove the frame is
/// actually delivered.
#[test]
fn a_title_carrying_switch_is_not_replaced_by_a_later_refresh() {
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        64,
        Arc::new(AtomicBool::new(false)),
    );
    let output = pane_output_channel();

    let terminal = OuterTerminal::resolve(
        &rmux_core::OptionStore::new(),
        crate::outer_terminal::OuterTerminalContext::from_pairs(&[("TERM", "tmux-256color")]),
    );
    let update = crate::outer_terminal::ClientTitleUpdate {
        resolved: Some("QUEUED-TITLE"),
        path: crate::outer_terminal::ClientPathUpdate::Unread,
        previous: None,
    };
    let mut carrying = deep_coalescible_target(&output, 0, 32);
    carrying.live_pane = None;
    carrying.render_frame = terminal.render_client_title(update);
    carrying.client_title = terminal.rendered_client_title(update);
    carrying.outer_terminal = terminal;
    assert!(
        !carrying.render_frame.is_empty(),
        "the fixture terminal must advertise the title capability"
    );
    assert!(
        !carrying.is_coalescible_render_refresh(),
        "a frame that carries OSC 0 must not be a replaceable refresh"
    );
    // It is still a re-render of the same pane. Only queue replaceability is
    // withdrawn — live-output continuity (`preserves_live_output`, exercised on
    // Unix in pane_io::tests) must not be, or a title change would drop the
    // pane's buffered kitty/sixel passthroughs.
    assert!(
        carrying.is_plain_render_refresh(),
        "withdrawing replaceability must not make this look like a pane switch"
    );

    sender
        .send(AttachControl::switch(carrying))
        .expect("title switch fits");
    // The follow-up deduplicated the title away, exactly as the next render does.
    let mut follow_up = deep_coalescible_target(&output, 1, 32);
    follow_up.live_pane = None;
    sender
        .send(AttachControl::switch(follow_up))
        .expect("follow-up refresh fits");

    let AttachControl::Switch(first) = receiver.try_recv().expect("the title switch is queued")
    else {
        panic!("expected a switch control");
    };
    let first = first.into_target_with_count().0;
    assert_eq!(
        String::from_utf8_lossy(&first.render_frame),
        "\u{1b}]0;QUEUED-TITLE\u{7}",
        "the title-carrying frame must reach the client"
    );

    let AttachControl::Switch(second) = receiver.try_recv().expect("the follow-up is queued")
    else {
        panic!("expected a switch control");
    };
    assert_eq!(second.into_target_with_count().0.render_frame[0], 1);
}

/// Withdrawing queue replaceability must not cost the pane's live output.
///
/// `preserves_live_output` decides whether the passthroughs buffered behind the
/// current target (kitty graphics, sixel) are re-emitted or dropped as if the
/// client had switched panes. A title-carrying frame re-renders the very same
/// pane, so they must survive — coupling that decision to the queue gate would
/// silently drop graphics on every title change (issue #182).
///
/// `pane_io::tests` covers the same invariant on Unix; this keeps it under
/// Windows native authority, where that module is not compiled.
#[test]
fn a_title_carrying_refresh_keeps_the_pane_live_output() {
    let output = pane_output_channel();
    let mut current = deep_coalescible_target(&output, 0, 32);
    current.live_pane = None;
    let current =
        crate::pane_io::wire::open_attach_target(current, false).expect("the current target opens");

    let terminal = OuterTerminal::resolve(
        &rmux_core::OptionStore::new(),
        crate::outer_terminal::OuterTerminalContext::from_pairs(&[("TERM", "tmux-256color")]),
    );
    let mut carrying = deep_coalescible_target(&output, 1, 32);
    carrying.live_pane = None;
    carrying.client_title =
        terminal.rendered_client_title(crate::outer_terminal::ClientTitleUpdate {
            resolved: Some("LIVE-OUTPUT-TITLE"),
            path: crate::outer_terminal::ClientPathUpdate::Unread,
            previous: None,
        });
    assert!(!carrying.is_coalescible_render_refresh());

    assert!(
        crate::pane_io::control::preserves_live_output(&current, &carrying),
        "a title-carrying refresh of the same pane must keep its live output"
    );
}

#[test]
fn popup_burst_accounts_retained_capacity_and_weighted_reservations() {
    for drop_receiver in [false, true] {
        let (inner, mut receiver) = mpsc::unbounded_channel();
        let backlog = Arc::new(AtomicUsize::new(0));
        let closing = Arc::new(AtomicBool::new(false));
        let sender = AttachControlSender::new(inner, Arc::clone(&backlog), 4, Arc::clone(&closing));
        let frame = |generation| {
            let mut bytes = Vec::with_capacity(65_536);
            bytes.resize(31_244, generation as u8);
            OverlayFrame::persistent(bytes, 3, generation)
        };
        sender.send_popup_overlay(7, frame(0)).unwrap();
        let AttachControl::Overlay(in_flight) = receive_materialized(&mut receiver, &backlog)
        else {
            panic!("overlay")
        };
        assert_eq!(backlog.load(Ordering::Acquire), 1);
        let mut retained_high_water = 0;
        let mut replacement_high_water = 0;
        for generation in 1..=63 {
            sender.send_popup_overlay(7, frame(generation)).unwrap();
            assert_eq!(receiver.len(), 1);
            let slot = sender
                .send_state
                .lock()
                .unwrap()
                .open_coalesced_popup_overlay
                .as_ref()
                .unwrap()
                .upgrade()
                .unwrap();
            let (len, capacity, units, overlap) = slot.retained_allocation_for_test();
            assert_eq!((len, capacity, units), (31_244, 65_536, 1));
            retained_high_water = retained_high_water.max(capacity + in_flight.frame.capacity());
            replacement_high_water =
                replacement_high_water.max(overlap + in_flight.frame.capacity());
            assert_eq!(backlog.load(Ordering::Acquire), 2);
            assert!(!closing.load(Ordering::SeqCst));
        }
        assert_eq!(retained_high_water, 131_072);
        assert_eq!(replacement_high_water, 196_608);
        if drop_receiver {
            drop(receiver);
        } else {
            let AttachControl::Overlay(latest) = receive_materialized(&mut receiver, &backlog)
            else {
                panic!("overlay")
            };
            assert_eq!(latest.overlay_generation, 63);
            assert!(latest.frame.iter().all(|byte| *byte == 63));
            assert_eq!(backlog.load(Ordering::Acquire), 2);
            drop(latest);
            assert!(receiver.is_empty());
        }
        assert_eq!(backlog.load(Ordering::Acquire), 1);
        drop(in_flight);
        assert_eq!(backlog.load(Ordering::Acquire), 0);
        eprintln!("popup allocations: pending+inflight_peak={retained_high_water} replace_overlap+inflight_peak={replacement_high_water} drop_receiver={drop_receiver} final_units=0");
    }
    let (inner, mut receiver) = mpsc::unbounded_channel();
    let backlog = Arc::new(AtomicUsize::new(0));
    let sender = AttachControlSender::new(
        inner,
        Arc::clone(&backlog),
        4,
        Arc::new(AtomicBool::new(false)),
    );
    for (generation, bytes, units) in [
        (1, 32, 1),
        (2, AttachControl::BACKLOG_UNIT_BYTES, 2),
        (3, 32, 1),
    ] {
        sender
            .send_popup_overlay(7, OverlayFrame::persistent(vec![0; bytes], 3, generation))
            .unwrap();
        assert_eq!(backlog.load(Ordering::Acquire), units);
    }
    drop(receive_materialized(&mut receiver, &backlog));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}
