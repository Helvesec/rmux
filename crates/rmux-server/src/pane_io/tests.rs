use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use rmux_core::events::OutputCursorItem;
use rmux_core::{OptionStore, PaneGeometry};
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, KeyDispatched,
    NewSessionRequest, Request, Response, SessionName, TerminalSize,
};
use rmux_pty::PtyPair;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use super::control::{apply_pending_attach_controls, PendingAttachAction};
use super::wire::open_attach_target;
use super::wire::recv_pane_output;
use super::{
    forward_attach, forward_attach_passthrough, pane_output_channel,
    pane_output_channel_with_limits, process_socket_messages, should_emit_overlay, AttachControl,
    AttachTarget, LiveAttachInputContext, OverlayFrame,
};
use crate::handler::RequestHandler;
use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};

mod persistent_overlay;

#[test]
fn overlay_generation_rejects_stale_clears_after_switches_or_newer_overlays() {
    let mut current_overlay_generation = 0;

    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert_eq!(current_overlay_generation, 1);

    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert!(should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 2)
    ));

    assert!(!should_emit_overlay(
        0,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 1)
    ));
    assert!(!should_emit_overlay(
        1,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 0, 3)
    ));
}

#[tokio::test]
async fn pane_output_receiver_reports_lag_and_resumes_from_oldest_retained_event() {
    let sender = pane_output_channel_with_limits(1, 32);
    let mut receiver = sender.subscribe();

    sender.send(b"first".to_vec());
    sender.send(b"second".to_vec());

    let OutputCursorItem::Gap(gap) = recv_pane_output(&mut receiver)
        .await
        .expect("receive explicit output gap")
    else {
        panic!("slow receiver should observe a cursor gap");
    };
    assert_eq!(gap.expected_sequence(), 0);
    assert_eq!(gap.resume_sequence(), 1);
    assert_eq!(gap.missed_events(), 1);
    assert_eq!(gap.missed_range(), 0..1);
    assert_eq!(gap.recent_snapshot().bytes(), b"firstsecond");
    assert_eq!(gap.recent_snapshot().oldest_sequence(), Some(0));
    assert_eq!(gap.recent_snapshot().newest_sequence(), Some(1));

    let OutputCursorItem::Event(event) = recv_pane_output(&mut receiver)
        .await
        .expect("receive oldest retained output event")
    else {
        panic!("receiver should resume with the oldest retained event");
    };
    assert_eq!(event.sequence(), 1);
    assert_eq!(event.bytes(), b"second");
}

#[tokio::test]
async fn typed_keystroke_wire_reaches_stub_and_acknowledges() {
    let proof_root =
        std::env::temp_dir().join(format!("rmux-step02-protocol-{}", std::process::id()));
    std::fs::create_dir_all(&proof_root).expect("create /tmp check root");

    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(
            attach_pid,
            SessionName::new("alpha").expect("valid session name"),
            control_tx,
        )
        .await;

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let keystroke = AttachedKeystroke::new(b"\x1b[A".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode typed keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut locked = true;
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid,
    };

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        &mut pending_input,
        &mut locked,
    )
    .await
    .expect("process typed keystroke");

    let mut ack_bytes = [0_u8; 64];
    let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut ack_bytes))
        .await
        .expect("ack read should not time out")
        .expect("read ack");
    let mut ack_decoder = AttachFrameDecoder::new();
    ack_decoder.push_bytes(&ack_bytes[..bytes_read]);
    assert_eq!(
        ack_decoder.next_message().expect("decode ack"),
        Some(AttachMessage::KeyDispatched(KeyDispatched::new(3)))
    );

    std::fs::remove_dir_all(proof_root).expect("remove /tmp check root");
}

#[tokio::test]
async fn mouse_keystroke_wire_does_not_error_or_drop_the_attach() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let (control_tx, _control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name, control_tx)
        .await;

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let keystroke = AttachedKeystroke::new(b"\x1b[<0;10;10M".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode mouse keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut locked = false;
    let live_input = LiveAttachInputContext {
        handler: Arc::clone(&handler),
        attach_pid,
    };

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        &mut pending_input,
        &mut locked,
    )
    .await
    .expect("process mouse keystroke");

    let mut ack_bytes = [0_u8; 128];
    let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut ack_bytes))
        .await
        .expect("ack read should not time out")
        .expect("read ack");
    let mut ack_decoder = AttachFrameDecoder::new();
    ack_decoder.push_bytes(&ack_bytes[..bytes_read]);
    assert_eq!(
        ack_decoder.next_message().expect("decode ack"),
        Some(AttachMessage::KeyDispatched(KeyDispatched::new(11)))
    );
}

#[tokio::test]
async fn forward_attach_emits_stop_sequence_when_processing_errors() {
    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let outer_terminal =
        OuterTerminal::resolve(&OptionStore::default(), OuterTerminalContext::default());
    let expected_stop = outer_terminal.attach_stop_sequence();
    let target = AttachTarget {
        session_name: SessionName::new("alpha").expect("valid session name"),
        pane_master,
        pane_output: pane_output_channel(),
        render_frame: Vec::new(),
        outer_terminal,
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };
    let invalid_initial_socket_bytes =
        encode_attach_message(&AttachMessage::Lock("unexpected".to_owned()))
            .expect("encode unexpected lock frame");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let result = forward_attach(
        stream,
        target,
        invalid_initial_socket_bytes,
        shutdown_rx,
        control_rx,
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
    )
    .await;
    assert!(result.is_err(), "invalid attach input should fail");

    let mut collected = Vec::new();
    let mut frame_bytes = [0_u8; 4096];
    loop {
        let bytes_read = tokio::time::timeout(Duration::from_secs(1), peer.read(&mut frame_bytes))
            .await
            .expect("peer read should not time out")
            .expect("read peer bytes");
        if bytes_read == 0 {
            break;
        }
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while let Some(message) = decoder.next_message().expect("decode attach frame") {
            if let AttachMessage::Data(bytes) = message {
                collected.extend_from_slice(&bytes);
            }
        }
    }

    assert!(
        collected
            .windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "attach stop sequence should be emitted on attach failure"
    );
}

fn test_attach_target(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
) -> AttachTarget {
    test_attach_target_with_output(
        session_name,
        render_frame,
        persistent_overlay_state_id,
        pane_output_channel(),
        false,
    )
}

fn test_attach_target_with_output(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
    pane_output: super::types::PaneOutputSender,
    kitty_graphics_passthrough: bool,
) -> AttachTarget {
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    AttachTarget {
        session_name: session_name.clone(),
        pane_master,
        pane_output,
        render_frame: render_frame.to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough,
        persistent_overlay_state_id,
        live_pane: None,
        active_pane_id: None,
    }
}

#[tokio::test]
async fn pending_switch_action_reports_target_change_for_status_reschedule() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target =
        open_attach_target(test_attach_target(&alpha, b"BASE-A", None)).expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &beta, b"BASE-B", None,
        )))
        .expect("send switch control");

    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &mut current_target,
        &stream,
        &mut render_generation,
        &mut overlay_generation,
        &mut persistent_overlay,
        &mut persistent_overlay_visible,
        &mut persistent_overlay_state_id,
        &mut locked,
    )
    .await
    .expect("apply pending switch");

    assert!(matches!(
        action,
        PendingAttachAction::Continue {
            target_changed: true
        }
    ));
    assert_eq!(current_target.session_name, beta);
    let refresh = read_attach_data_until(&mut peer, b"BASE-B").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "switch should render the target frame"
    );
}

async fn read_attach_data_until(peer: &mut tokio::net::UnixStream, needle: &[u8]) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut collected = Vec::new();
        let mut frame_bytes = [0_u8; 4096];
        let mut decoder = AttachFrameDecoder::new();
        loop {
            let bytes_read = peer.read(&mut frame_bytes).await.expect("read peer bytes");
            assert!(bytes_read > 0, "attach stream closed before expected data");
            decoder.push_bytes(&frame_bytes[..bytes_read]);
            while let Some(message) = decoder.next_message().expect("decode attach frame") {
                if let AttachMessage::Data(bytes) = message {
                    collected.extend_from_slice(&bytes);
                }
            }
            if collected
                .windows(needle.len())
                .any(|window| window == needle)
            {
                break collected;
            }
        }
    })
    .await
    .expect("timed out waiting for attach data")
}

#[tokio::test]
async fn forward_attach_plain_refresh_does_not_clear_the_screen() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"BASE-1",
            None,
        )))
        .expect("send refreshed attach target");

    let refresh = read_attach_data_until(&mut peer, b"BASE-1").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        !refresh_text.contains("\x1b[2J"),
        "plain pane-output refresh must not clear the whole terminal: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_preserves_persistent_overlay_across_stateful_switch_refreshes() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target(&session_name, b"BASE-0", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU-OLD".to_vec(),
            0,
            1,
            7,
        )))
        .expect("send initial persistent overlay");
    let overlay = read_attach_data_until(&mut peer, b"MENU-OLD").await;
    assert!(
        String::from_utf8_lossy(&overlay).contains("MENU-OLD"),
        "persistent overlay should be visible before the refresh"
    );

    control_tx
        .send(AttachControl::AdvancePersistentOverlayState(8))
        .expect("send overlay state advance");
    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"BASE-1",
            Some(8),
        )))
        .expect("send refreshed attach target");

    let refresh = read_attach_data_until(&mut peer, b"MENU-OLD").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
            refresh_text.contains("BASE-1") && refresh_text.contains("MENU-OLD"),
            "stateful choose-tree refresh should compose the refreshed base and cached overlay in one render frame: {refresh_text:?}"
        );
    assert!(
            !refresh_text.contains("\x1b[2J"),
            "stateful choose-tree refresh must not clear to the base pane before the replacement overlay: {refresh_text:?}"
        );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_emits_display_panes_overlay_for_prefix_q_keystrokes() {
    let handler = Arc::new(RequestHandler::new());
    let attach_pid = std::process::id();
    let session_name = SessionName::new("alpha").expect("valid session name");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));
    let split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));
    let set_option = handler
        .handle(Request::SetOption(rmux_proto::SetOptionRequest {
            scope: rmux_proto::ScopeSelector::Session(session_name.clone()),
            option: rmux_proto::OptionName::DisplayPanesTime,
            value: "5000".to_owned(),
            mode: rmux_proto::SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set_option, Response::SetOption(_)));

    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;

    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let target = AttachTarget {
        session_name: session_name.clone(),
        pane_master,
        pane_output: pane_output_channel(),
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };

    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid,
    };

    let attach_task = tokio::spawn(async move {
        forward_attach(
            stream,
            target,
            Vec::new(),
            shutdown_rx,
            control_rx,
            closing,
            Arc::new(AtomicU64::new(0)),
            live_input,
        )
        .await
    });

    let encoded = encode_attach_message(&AttachMessage::Keystroke(AttachedKeystroke::new(
        b"\x02q".to_vec(),
    )))
    .expect("encode prefix q");
    tokio::io::AsyncWriteExt::write_all(&mut peer, &encoded)
        .await
        .expect("send prefix q");

    let mut collected = Vec::new();
    let mut saw_ack = false;
    let mut frame_bytes = [0_u8; 4096];
    let mut decoder = AttachFrameDecoder::new();
    while let Ok(Ok(bytes_read)) =
        tokio::time::timeout(Duration::from_millis(250), peer.read(&mut frame_bytes)).await
    {
        if bytes_read == 0 {
            break;
        }
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while let Some(message) = decoder.next_message().expect("decode attach frame") {
            match message {
                AttachMessage::Data(bytes) => collected.extend_from_slice(&bytes),
                AttachMessage::KeyDispatched(_) => saw_ack = true,
                _ => {}
            }
        }
        if collected
            .windows(b"\x1b[?25l".len())
            .any(|window| window == b"\x1b[?25l")
        {
            break;
        }
    }

    assert!(
        saw_ack,
        "prefix q should at least be acknowledged by the attach stream"
    );
    assert!(
        collected
            .windows(b"\x1b[?25l".len())
            .any(|window| window == b"\x1b[?25l"),
        "prefix q should emit a display-panes overlay frame, got: {:?}",
        String::from_utf8_lossy(&collected)
    );

    peer.shutdown().await.expect("close client peer");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_passthrough_forwards_pane_output_verbatim_without_alt_screen() {
    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let session_name = SessionName::new("alpha").expect("valid session name");

    let pane_output = pane_output_channel();
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let target = AttachTarget {
        session_name: session_name.clone(),
        pane_master,
        pane_output: pane_output.clone(),
        // Empty render_frame so the first bytes we see on the peer are
        // strictly the pane-output bytes we push below.
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };

    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach_passthrough(
        stream,
        target,
        Vec::new(),
        shutdown_rx,
        control_rx,
        closing.clone(),
        Arc::new(AtomicU64::new(0)),
        live_input,
    ));

    // Give the bypass loop time to enter its select before we publish
    // — otherwise the subscribe() inside open_attach_target starts
    // "from now" and would miss this byte.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let _ = pane_output.send(b"hello passthrough\r\n".to_vec());

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while collected
        .windows(b"hello passthrough".len())
        .all(|window| window != b"hello passthrough")
    {
        let mut buf = [0_u8; 4096];
        let read = tokio::time::timeout(deadline - tokio::time::Instant::now(), peer.read(&mut buf))
            .await
            .expect("peer read should not time out")
            .expect("peer read");
        if read == 0 {
            break;
        }
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&buf[..read]);
        while let Some(AttachMessage::Data(bytes)) =
            decoder.next_message().expect("decode attach frame")
        {
            collected.extend_from_slice(&bytes);
        }
    }

    assert!(
        collected
            .windows(b"hello passthrough".len())
            .any(|window| window == b"hello passthrough"),
        "pane bytes must be forwarded verbatim, got: {:?}",
        String::from_utf8_lossy(&collected)
    );
    assert!(
        !collected
            .windows(b"\x1b[?1049h".len())
            .any(|window| window == b"\x1b[?1049h"),
        "passthrough must not put the host terminal into alt-screen, got: {:?}",
        String::from_utf8_lossy(&collected)
    );

    closing.store(true, std::sync::atomic::Ordering::SeqCst);
    peer.shutdown().await.expect("close peer");
    let _ = tokio::time::timeout(Duration::from_secs(2), attach_task)
        .await
        .expect("attach task join did not time out");
}

#[tokio::test]
async fn forward_attach_passthrough_replays_target_on_attach_control_switch() {
    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let session_name = SessionName::new("alpha").expect("valid session name");

    let pane_output_one = pane_output_channel();
    let pane_output_two = pane_output_channel();
    let pty_one = PtyPair::open().expect("open pty pair 1");
    let pty_two = PtyPair::open().expect("open pty pair 2");

    let target_one = AttachTarget {
        session_name: session_name.clone(),
        pane_master: pty_one.into_master(),
        pane_output: pane_output_one.clone(),
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };
    let target_two = AttachTarget {
        session_name: session_name.clone(),
        pane_master: pty_two.into_master(),
        pane_output: pane_output_two.clone(),
        // A distinctive render_frame for window 2 so we can prove the
        // switch handler used the new target's bytes (not stale ones).
        render_frame: b"WINDOW-TWO-PAINT".to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };

    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach_passthrough(
        stream,
        target_one,
        Vec::new(),
        shutdown_rx,
        control_rx,
        closing.clone(),
        Arc::new(AtomicU64::new(0)),
        live_input,
    ));

    // Trigger a switch and let the loop process it.
    control_tx
        .send(AttachControl::switch(target_two))
        .expect("switch send");
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Once switched, output on window 2's channel must reach the
    // client; output on window 1's channel must not.
    let _ = pane_output_two.send(b"WINDOW-TWO-OUTPUT".to_vec());
    let _ = pane_output_one.send(b"STALE-WINDOW-ONE".to_vec());

    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while collected
        .windows(b"WINDOW-TWO-OUTPUT".len())
        .all(|window| window != b"WINDOW-TWO-OUTPUT")
    {
        let mut buf = [0_u8; 4096];
        let read = tokio::time::timeout(deadline - tokio::time::Instant::now(), peer.read(&mut buf))
            .await
            .expect("peer read should not time out")
            .expect("peer read");
        if read == 0 {
            break;
        }
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&buf[..read]);
        while let Some(AttachMessage::Data(bytes)) =
            decoder.next_message().expect("decode attach frame")
        {
            collected.extend_from_slice(&bytes);
        }
    }

    let contains = |needle: &[u8]| {
        collected
            .windows(needle.len())
            .any(|window| window == needle)
    };
    assert!(
        contains(b"WINDOW-TWO-PAINT"),
        "switch must emit new target's render_frame, got: {:?}",
        String::from_utf8_lossy(&collected)
    );
    assert!(
        contains(b"\x1b[2J"),
        "switch must clear the host screen before painting the new target, got: {:?}",
        String::from_utf8_lossy(&collected)
    );
    assert!(
        contains(b"WINDOW-TWO-OUTPUT"),
        "post-switch pane output must reach the client, got: {:?}",
        String::from_utf8_lossy(&collected)
    );
    assert!(
        !contains(b"STALE-WINDOW-ONE"),
        "output on the previous window's channel must not reach the client after switch, got: {:?}",
        String::from_utf8_lossy(&collected)
    );
    assert!(
        contains(b"\x1b]0;rmux: alpha\x07"),
        "switch must emit a rmux-tagged OSC 0 title so the host bar doesn't keep the previous window's title, got: {:?}",
        String::from_utf8_lossy(&collected)
    );

    closing.store(true, std::sync::atomic::Ordering::SeqCst);
    peer.shutdown().await.expect("close peer");
    let _ = tokio::time::timeout(Duration::from_secs(2), attach_task)
        .await
        .expect("attach task join did not time out");
}

#[cfg(unix)]
#[tokio::test]
async fn forward_attach_passthrough_winches_new_window_on_switch() {
    use rmux_pty::ChildCommand;

    let handler = Arc::new(RequestHandler::new());
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let session_name = SessionName::new("alpha").expect("valid session name");

    // Window 1: idle PTY with no child. The forwarder starts here and
    // we switch away before doing anything with it.
    let pty_one = PtyPair::open().expect("open pty pair 1");

    // Window 2: a real shell spawned under a fresh PTY via the same
    // `ChildCommand::spawn` rmux uses in production. The shell traps
    // WINCH and writes a sentinel to its stdout (= the slave) so we
    // can observe delivery on the master.
    let spawned = ChildCommand::new("/bin/sh")
        .arg("-c")
        .arg(
            "trap 'printf RMUX_WINCH_OK' WINCH; \
             i=0; while [ \"$i\" -lt 100 ]; do sleep 0.05; i=$((i+1)); done",
        )
        .spawn()
        .expect("spawn /bin/sh under window 2 pty");
    let (master_two, mut child_two) = spawned.into_parts();
    // A second master handle so the reader thread doesn't fight the
    // attach target for the fd.
    let master_two_reader = master_two
        .try_clone()
        .expect("clone window 2 master for reader");

    // Wait for the shell to claim the slave's fg pgrp.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while rmux_os::process::unix::foreground_pid(master_two.as_fd()).is_none() {
        if tokio::time::Instant::now() >= deadline {
            let _ = child_two.wait();
            panic!("child shell did not claim window 2's fg pgrp within 2s");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Give the shell a beat to install its WINCH trap after exec.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let pane_output_one = pane_output_channel();
    let pane_output_two = pane_output_channel();

    let target_one = AttachTarget {
        session_name: session_name.clone(),
        pane_master: pty_one.into_master(),
        pane_output: pane_output_one,
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };
    let target_two = AttachTarget {
        session_name: session_name.clone(),
        pane_master: master_two,
        pane_output: pane_output_two,
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        kitty_graphics_passthrough: false,
        persistent_overlay_state_id: None,
        live_pane: None,
        active_pane_id: None,
    };

    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach_passthrough(
        stream,
        target_one,
        Vec::new(),
        shutdown_rx,
        control_rx,
        closing.clone(),
        Arc::new(AtomicU64::new(0)),
        live_input,
    ));

    // The switch is the SIGWINCH delivery point.
    control_tx
        .send(AttachControl::switch(target_two))
        .expect("switch send");

    // Read from window 2's master on a blocking thread; the shell
    // trap writes the sentinel via the slave and the master receives
    // it. Use a timeout so a missed SIGWINCH surfaces as a clean fail.
    let (sentinel_tx, sentinel_rx) = tokio::sync::oneshot::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut collected = Vec::new();
        let mut buffer = [0_u8; 256];
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match master_two_reader.io().read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    collected.extend_from_slice(&buffer[..n]);
                    if collected
                        .windows(b"RMUX_WINCH_OK".len())
                        .any(|window| window == b"RMUX_WINCH_OK")
                    {
                        break;
                    }
                }
            }
        }
        let _ = sentinel_tx.send(collected);
    });

    let collected = tokio::time::timeout(Duration::from_secs(4), sentinel_rx)
        .await
        .expect("reader thread did not finish in time")
        .expect("reader thread did not send");
    let saw_sentinel = collected
        .windows(b"RMUX_WINCH_OK".len())
        .any(|window| window == b"RMUX_WINCH_OK");

    closing.store(true, std::sync::atomic::Ordering::SeqCst);
    peer.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), attach_task).await;
    let _ = child_two.kill(rmux_pty::Signal::KILL);
    let _ = child_two.wait();

    assert!(
        saw_sentinel,
        "passthrough switch must deliver SIGWINCH to the new window's fg pgrp; \
         got master read: {:?}",
        String::from_utf8_lossy(&collected)
    );
}
