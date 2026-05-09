use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;

use rmux_core::events::OutputCursorItem;
use rmux_core::OptionStore;
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke, KeyDispatched,
    NewSessionRequest, Request, Response, SessionName, TerminalSize,
};
use rmux_pty::PtyPair;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};

use super::wire::recv_pane_output;
use super::{
    forward_attach, pane_output_channel, pane_output_channel_with_limits, process_socket_messages,
    should_emit_overlay, AttachControl, AttachTarget, LiveAttachInputContext, OverlayFrame,
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
        persistent_overlay_state_id: None,
        live_pane: None,
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
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    AttachTarget {
        session_name: session_name.clone(),
        pane_master,
        pane_output: pane_output_channel(),
        render_frame: render_frame.to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        persistent_overlay_state_id,
        live_pane: None,
    }
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
        persistent_overlay_state_id: None,
        live_pane: None,
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
