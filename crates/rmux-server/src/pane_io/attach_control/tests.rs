use super::*;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::live_render::LivePaneRender;
use crate::pane_io::types::{pane_output_channel, AttachTarget, PaneOutputSender};
use rmux_core::PaneGeometry;
use rmux_proto::TerminalSize;
fn deep_coalescible_target(
    output: &PaneOutputSender,
    marker: u8,
    render_len: usize,
) -> AttachTarget {
    let size = TerminalSize { cols: 10, rows: 4 };
    let session = rmux_core::Session::new(
        rmux_proto::SessionName::new(format!("switch-{marker}")).expect("valid session name"),
        size,
    );
    let pane = session
        .window()
        .active_pane()
        .expect("session has an active pane")
        .clone();
    let transcript = crate::pane_transcript::PaneTranscript::shared(64, size);
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .append_bytes(&[marker]);
    let live_pane = Some(
        LivePaneRender::new_from_transcript(
            transcript,
            session,
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
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, size.cols, size.rows),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane,
    }
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
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
    };
    assert_eq!(AttachControl::switch(target).backlog_units(), 2);
}
