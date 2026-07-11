use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_core::events::OutputCursorItem;
use rmux_core::{OptionStore, PaneGeometry, TerminalPassthrough};
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand,
    AttachedKeystroke, KeyDispatched, NewSessionRequest, PaneTarget, Request, Response,
    SessionName, TerminalSize,
};
use rmux_pty::PtyPair;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::attach_transport::AttachTransport;
use super::control::{
    apply_pending_attach_controls, coalesce_render_switches, PendingAttachAction,
};
use super::wire::open_attach_target;
use super::wire::recv_pane_output;
use super::{
    clear_close_pane_output_after_refresh_if_target_changed, consume_predicted_echo,
    forward_attach, is_predictable_local_echo, pane_output_channel,
    pane_output_channel_with_limits, predictable_local_echo_prefix_len, process_socket_messages,
    should_emit_overlay, AttachControl, AttachTarget, LiveAttachInputContext, OverlayFrame,
    PredictedEcho,
};
use crate::handler::RequestHandler;
use crate::outer_terminal::{OuterTerminal, OuterTerminalContext};
use crate::renderer::PaneRenderDeltaFrame;

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
    assert_eq!(current_overlay_generation, 2);

    assert!(should_emit_overlay(
        1,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 2, 3)
    ));
    assert_eq!(current_overlay_generation, 3);

    assert!(!should_emit_overlay(
        2,
        &mut current_overlay_generation,
        &OverlayFrame::new(Vec::new(), 1, 4)
    ));
}

#[test]
fn target_change_clears_deferred_pane_output_close() {
    let mut close_after_refresh = true;

    clear_close_pane_output_after_refresh_if_target_changed(true, &mut close_after_refresh);

    assert!(
        !close_after_refresh,
        "a deferred close belongs to the old attach target and must not apply after a switch"
    );
}

#[test]
fn same_target_keeps_deferred_pane_output_close() {
    let mut close_after_refresh = true;

    clear_close_pane_output_after_refresh_if_target_changed(false, &mut close_after_refresh);

    assert!(close_after_refresh);
}

#[test]
fn predicted_local_echo_accepts_only_single_printable_bytes() {
    assert!(is_predictable_local_echo(b"a"));
    assert!(is_predictable_local_echo(b"abc123"));
    assert!(is_predictable_local_echo(b" "));
    assert!(is_predictable_local_echo(b"~"));
    assert!(!is_predictable_local_echo(b"\n"));
    assert!(!is_predictable_local_echo(b"\x1b"));
    assert!(!is_predictable_local_echo(b"0123456789abcdefg"));
    assert!(!is_predictable_local_echo("é".as_bytes()));
}

#[test]
fn predicted_local_echo_accepts_printable_prefix_before_enter() {
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\r"), 7);
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\n"), 7);
    assert_eq!(predictable_local_echo_prefix_len(b"PING123\t"), 0);
    assert_eq!(predictable_local_echo_prefix_len(b"\r"), 0);
}

#[test]
fn predicted_local_echo_consumes_exact_pty_echo_once() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let mut target =
        open_attach_target(test_attach_target(&alpha, b"", None), false).expect("open target");

    target.predicted_echo.extend(b"xyz");
    assert_eq!(
        consume_predicted_echo(&mut target, b"xyz"),
        PredictedEcho::Consumed
    );
    assert!(target.predicted_echo.is_empty());

    target.predicted_echo.extend(b"x");
    assert_eq!(
        consume_predicted_echo(&mut target, b"y"),
        PredictedEcho::Mismatch
    );
    assert!(target.predicted_echo.is_empty());

    target.predicted_echo.extend(b"x");
    assert_eq!(
        consume_predicted_echo(&mut target, b"xy"),
        PredictedEcho::Mismatch
    );
    assert!(target.predicted_echo.is_empty());
}

#[test]
fn stale_predicted_local_echo_expires_without_pty_echo() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let mut target =
        open_attach_target(test_attach_target(&alpha, b"", None), false).expect("open target");

    target.predicted_echo.extend(b"secret");
    target.predicted_echo_started_at =
        Some(Instant::now() - super::PREDICTED_LOCAL_ECHO_TIMEOUT * 2);

    assert_eq!(
        consume_predicted_echo(&mut target, b"visible"),
        PredictedEcho::NoPrediction
    );
    assert!(target.predicted_echo.is_empty());
    assert!(target.predicted_echo_started_at.is_none());
}

#[tokio::test]
async fn live_render_frame_uses_render_message_for_capable_clients() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let target = test_attach_target(&alpha, b"", None);
    let mut target = open_attach_target(target, true).expect("open attach target");
    let frame = PaneRenderDeltaFrame::new(b"live".to_vec(), None);
    let (stream, mut peer) = tokio::io::duplex(1024);
    let stream = AttachTransport::from_io(stream);

    super::emit_live_render_frame(&stream, &mut target, &frame, true)
        .await
        .expect("emit live render frame");

    let mut bytes = [0_u8; 128];
    let count = peer.read(&mut bytes).await.expect("read emitted frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&bytes[..count]);

    assert!(matches!(
        decoder.next_message().expect("decode emitted frame"),
        Some(AttachMessage::Render(bytes)) if bytes.ends_with(b"live")
    ));
}

#[tokio::test]
async fn live_render_delta_uses_data_message_for_stateful_frames() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let target = test_attach_target(&alpha, b"", None);
    let mut target = open_attach_target(target, true).expect("open attach target");
    let frame = PaneRenderDeltaFrame::new(b"delta".to_vec(), None);
    let (stream, mut peer) = tokio::io::duplex(1024);
    let stream = AttachTransport::from_io(stream);

    super::emit_live_render_frame(&stream, &mut target, &frame, false)
        .await
        .expect("emit live render frame");

    let mut bytes = [0_u8; 128];
    let count = peer.read(&mut bytes).await.expect("read emitted frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&bytes[..count]);

    assert!(matches!(
        decoder.next_message().expect("decode emitted frame"),
        Some(AttachMessage::Data(bytes)) if bytes.ends_with(b"delta")
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
    let stream = AttachTransport::from(stream);
    let keystroke = AttachedKeystroke::new(b"\x1b[A".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode typed keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut active_emit_cache = None;
    let mut locked = true;
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid,
    };

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        &mut pending_input,
        &mut active_emit_cache,
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
    let stream = AttachTransport::from(stream);
    let keystroke = AttachedKeystroke::new(b"\x1b[<0;10;10M".to_vec());
    let encoded = encode_attach_message(&AttachMessage::Keystroke(keystroke))
        .expect("encode mouse keystroke");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&encoded);
    let mut pending_input = Vec::new();
    let mut active_emit_cache = None;
    let mut locked = false;
    let live_input = LiveAttachInputContext {
        handler: Arc::clone(&handler),
        attach_pid,
    };

    process_socket_messages(
        &mut decoder,
        &stream,
        &live_input,
        None,
        &mut pending_input,
        &mut active_emit_cache,
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
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: SessionName::new("alpha").expect("valid session name"),
        input_target: PaneTarget::with_window(
            SessionName::new("alpha").expect("valid session name"),
            0,
            0,
        ),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: Vec::new(),
        outer_terminal,
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
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
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
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
            if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
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

#[tokio::test]
async fn detach_control_emits_stop_and_banner_in_one_data_frame() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
    let expected_stop = current_target.outer_terminal.attach_stop_sequence();
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::Detach)
        .expect("send detach control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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
    .expect("apply pending detach");

    assert!(matches!(action, PendingAttachAction::Exit(_)));

    let mut frame_bytes = [0_u8; 4096];
    let bytes_read = peer
        .read(&mut frame_bytes)
        .await
        .expect("read detach frame");
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame_bytes[..bytes_read]);
    let Some(AttachMessage::Data(bytes)) = decoder.next_message().expect("decode detach frame")
    else {
        panic!("detach should emit a data frame");
    };

    assert!(
        bytes
            .windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "detach data must contain attach-stop before close"
    );
    assert!(
        bytes
            .windows(b"[detached (from session alpha)]\r\n".len())
            .any(|window| window == b"[detached (from session alpha)]\r\n"),
        "detach data must contain detached banner"
    );
}

#[tokio::test]
async fn lock_control_emits_attach_stop_before_transferring_terminal_ownership() {
    let alpha = SessionName::new("alpha-lock-stop").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target =
        open_attach_target(test_attach_target(&alpha, b"BASE", None), false).expect("open target");
    let expected_stop = current_target.outer_terminal.attach_stop_sequence();
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();
    let command = AttachShellCommand::new(
        "lock-command".to_owned(),
        "/bin/sh".to_owned(),
        "/tmp".to_owned(),
    );
    control_tx
        .send(AttachControl::LockShellCommand(command.clone()))
        .expect("send lock control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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
    .expect("apply pending lock");
    assert!(matches!(action, PendingAttachAction::Continue { .. }));
    assert!(locked);

    let messages = tokio::time::timeout(Duration::from_secs(1), async {
        let mut decoder = AttachFrameDecoder::new();
        let mut messages = Vec::new();
        let mut bytes = [0_u8; 4096];
        while messages.len() < 2 {
            let read = peer.read(&mut bytes).await.expect("read lock frames");
            assert!(read > 0, "attach stream closed before lock frames");
            decoder.push_bytes(&bytes[..read]);
            while let Some(message) = decoder.next_message().expect("decode lock frame") {
                messages.push(message);
            }
        }
        messages
    })
    .await
    .expect("lock frames timed out");

    let AttachMessage::Data(stop) = &messages[0] else {
        panic!(
            "first lock frame must restore the outer terminal: {:?}",
            messages[0]
        );
    };
    assert!(
        stop.windows(expected_stop.len())
            .any(|window| window == expected_stop),
        "lock must emit the complete attach-stop sequence first"
    );
    assert_eq!(messages[1], AttachMessage::LockShellCommand(command));
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
    test_attach_target_with_protocols(
        session_name,
        render_frame,
        persistent_overlay_state_id,
        pane_output,
        kitty_graphics_passthrough,
        false,
    )
}

fn test_attach_target_with_protocols(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
    pane_output: super::types::PaneOutputSender,
    kitty_graphics_passthrough: bool,
    sixel_passthrough: bool,
) -> AttachTarget {
    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    AttachTarget {
        session_name: session_name.clone(),
        input_target: PaneTarget::with_window(session_name.clone(), 0, 0),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: render_frame.to_vec(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: kitty_graphics_passthrough || sixel_passthrough,
        kitty_graphics_passthrough,
        sixel_passthrough,
        persistent_overlay_state_id,
        live_pane: None,
    }
}

fn test_render_only_attach_target(session_name: &SessionName, render_frame: &[u8]) -> AttachTarget {
    test_render_only_attach_target_with_state(session_name, render_frame, None)
}

fn test_render_only_attach_target_with_state(
    session_name: &SessionName,
    render_frame: &[u8],
    persistent_overlay_state_id: Option<u64>,
) -> AttachTarget {
    let mut target = test_attach_target(session_name, render_frame, persistent_overlay_state_id);
    target.pane_master = None;
    target
}

#[test]
fn render_only_switches_coalesce_before_reliable_controls() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut deferred_controls = VecDeque::new();

    let first = test_render_only_attach_target(&alpha, b"first");
    let second = test_render_only_attach_target(&alpha, b"second");
    let third = test_render_only_attach_target(&alpha, b"third");
    control_tx
        .send(AttachControl::switch(second))
        .expect("queue second switch");
    control_tx
        .send(AttachControl::switch(third))
        .expect("queue third switch");
    control_tx
        .send(AttachControl::Detach)
        .expect("queue reliable detach");

    let control_backlog = AtomicUsize::new(0);
    let (coalesced, switch_count) = coalesce_render_switches(
        Box::new(first),
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
    );

    assert_eq!(coalesced.render_frame, b"third");
    assert_eq!(switch_count, 3);
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Detach)
    ));
}

#[test]
fn render_only_switch_coalescing_preserves_deferred_control_order() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut deferred_controls = VecDeque::from([AttachControl::Refresh]);

    let first = test_render_only_attach_target(&alpha, b"first");
    let second = test_render_only_attach_target(&alpha, b"second");
    control_tx
        .send(AttachControl::switch(second))
        .expect("queue render switch");
    control_tx
        .send(AttachControl::Detach)
        .expect("queue reliable detach");

    let control_backlog = AtomicUsize::new(0);
    let (coalesced, switch_count) = coalesce_render_switches(
        Box::new(first),
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
    );

    assert_eq!(coalesced.render_frame, b"second");
    assert_eq!(switch_count, 2);
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Refresh)
    ));
    assert!(matches!(
        deferred_controls.pop_front(),
        Some(AttachControl::Detach)
    ));
}

#[tokio::test]
async fn pending_switch_action_reports_target_change_for_status_reschedule() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
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

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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

#[tokio::test]
async fn pending_refresh_after_switch_preserves_target_change() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target = open_attach_target(test_attach_target(&alpha, b"BASE-A", None), false)
        .expect("open target");
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
    control_tx
        .send(AttachControl::Refresh)
        .expect("send refresh control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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
    .expect("apply pending switch and refresh");

    assert!(matches!(
        action,
        PendingAttachAction::Refresh {
            target_changed: true
        }
    ));
    assert_eq!(current_target.session_name, beta);
    let refresh = read_attach_data_until(&mut peer, b"BASE-B").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "switch should render before the refresh is scheduled"
    );
}

#[tokio::test]
async fn stale_persistent_switches_still_advance_render_generation() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let beta = SessionName::new("beta").expect("valid session name");
    let (stream, _peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let mut current_target =
        open_attach_target(test_attach_target(&alpha, b"BASE-A", Some(10)), false)
            .expect("open target");
    let mut render_generation = 41_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &beta,
            b"STALE-B",
            Some(9),
        )))
        .expect("send stale switch control");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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
    .expect("apply stale pending switch");

    assert!(matches!(action, PendingAttachAction::Write));
    assert_eq!(current_target.session_name, alpha);
    assert_eq!(render_generation, 42);
}

#[tokio::test]
async fn render_only_switch_forwards_pending_live_passthroughs() {
    let alpha = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let stream = AttachTransport::from(stream);
    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let pane_output = pane_output_channel();
    let mut initial =
        test_attach_target_with_output(&alpha, b"BASE-A", None, pane_output.clone(), true);
    initial.pane_master = None;
    let mut replacement =
        test_attach_target_with_output(&alpha, b"BASE-B", None, pane_output.clone(), true);
    replacement.pane_master = None;
    let mut current_target = open_attach_target(initial, false).expect("open target");
    let mut render_generation = 0_u64;
    let mut overlay_generation = 0_u64;
    let mut persistent_overlay = None::<Vec<u8>>;
    let mut persistent_overlay_visible = false;
    let mut persistent_overlay_state_id = current_target.persistent_overlay_state_id;
    let mut locked = false;
    let mut deferred_controls = VecDeque::new();

    pane_output.send_for_generation_with_passthroughs(
        None,
        b"image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAAA".to_vec(),
        )],
    );
    pane_output.send_for_generation_with_passthroughs(
        None,
        b"next-image".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;BBBB".to_vec(),
        )],
    );
    replacement.pane_output_start_sequence = 1;
    control_tx
        .send(AttachControl::switch(replacement))
        .expect("send render-only switch");

    let control_backlog = AtomicUsize::new(0);
    let action = apply_pending_attach_controls(
        &mut deferred_controls,
        Some(&mut control_rx),
        &control_backlog,
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

    assert!(matches!(action, PendingAttachAction::Write));
    let refresh = read_attach_data_until(&mut peer, b"Gf=100;AAAA").await;
    assert!(
        String::from_utf8_lossy(&refresh).contains("BASE-B"),
        "render-only switch should still write the replacement frame"
    );
    assert!(
        refresh
            .windows(b"\x1b_Gf=100;AAAA\x1b\\".len())
            .any(|window| window == b"\x1b_Gf=100;AAAA\x1b\\"),
        "render-only switch must not drop pending live passthroughs"
    );
    assert!(
        !refresh
            .windows(b"\x1b_Gf=100;BBBB\x1b\\".len())
            .any(|window| window == b"\x1b_Gf=100;BBBB\x1b\\"),
        "render-only switch must not duplicate passthroughs covered by the replacement receiver"
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
                if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
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
async fn forward_attach_exited_control_wins_over_closing_shutdown() {
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
        Arc::new(AtomicUsize::new(0)),
        Arc::clone(&closing),
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::Refresh)
        .expect("queue non-terminal control");
    control_tx
        .send(AttachControl::Exited)
        .expect("send exited control");
    closing.store(true, Ordering::SeqCst);
    shutdown_tx.send(()).expect("request attach shutdown");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    assert!(
        exited
            .windows(b"[exited]\r\n".len())
            .any(|window| window == b"[exited]\r\n"),
        "exited control must win over the closing shutdown race"
    );

    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should exit cleanly: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_exited_control_drains_final_output_and_passthrough_before_banner() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("exit-drain").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (_shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let pane_output = pane_output_channel();
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), true),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    #[cfg(windows)]
    {
        control_tx
            .send(AttachControl::Exited)
            .expect("send exited control");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = pane_output.send_for_generation_with_passthroughs(
        None,
        b"FINAL_TAIL".to_vec(),
        vec![TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;TAIL".to_vec(),
        )],
    );
    let _ = pane_output.send_for_generation(None, Vec::new());
    #[cfg(not(windows))]
    control_tx
        .send(AttachControl::Exited)
        .expect("send exited control");

    let exited = read_attach_data_until(&mut peer, b"[exited]\r\n").await;
    let tail = exited
        .windows(b"FINAL_TAIL".len())
        .position(|window| window == b"FINAL_TAIL")
        .expect("final pane output must be delivered");
    let passthrough = exited
        .windows(b"\x1b_Gf=100;TAIL\x1b\\".len())
        .position(|window| window == b"\x1b_Gf=100;TAIL\x1b\\")
        .expect("final passthrough must be delivered");
    let banner = exited
        .windows(b"[exited]\r\n".len())
        .position(|window| window == b"[exited]\r\n")
        .expect("exit banner must be delivered");
    assert!(tail < banner);
    assert!(passthrough < banner);

    assert!(attach_task.await.expect("attach task join").is_ok());
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
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
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
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
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
async fn forward_attach_counts_coalesced_switches_before_persistent_overlay() {
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
        test_render_only_attach_target(&session_name, b"BASE-0"),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-0").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-0"),
        "initial attach should render the base pane"
    );

    control_tx
        .send(AttachControl::switch(test_render_only_attach_target(
            &session_name,
            b"BASE-1",
        )))
        .expect("send prompt close refresh");
    control_tx
        .send(AttachControl::switch(test_render_only_attach_target(
            &session_name,
            b"BASE-2",
        )))
        .expect("send session mutation refresh");
    control_tx
        .send(AttachControl::switch(
            test_render_only_attach_target_with_state(&session_name, b"BASE-3", Some(8)),
        ))
        .expect("send mode-tree switch");
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU-NEW".to_vec(),
            3,
            1,
            8,
        )))
        .expect("send mode-tree overlay");

    let refresh = read_attach_data_until(&mut peer, b"MENU-NEW").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        refresh_text.contains("BASE-3") && refresh_text.contains("MENU-NEW"),
        "coalesced switch generation must still match the pending overlay: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_emits_overlay_control_frames() {
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
    let test_control_tx = control_tx.clone();
    handler
        .register_attach(attach_pid, session_name.clone(), control_tx)
        .await;

    let pty = PtyPair::open().expect("open pty pair");
    let pane_master = pty.into_master();
    let pane_output = pane_output_channel();
    let (pane_output_start_sequence, pane_output) = pane_output.subscribe_live_from_now();
    let target = AttachTarget {
        session_name: session_name.clone(),
        input_target: PaneTarget::with_window(session_name.clone(), 0, 0),
        pane_master: Some(pane_master),
        pane_output,
        pane_output_start_sequence,
        render_frame: Vec::new(),
        outer_terminal: OuterTerminal::resolve(
            &OptionStore::default(),
            OuterTerminalContext::default(),
        ),
        cursor_style: 0,
        active_pane_geometry: PaneGeometry::new(0, 0, 80, 24),
        raw_passthrough: false,
        kitty_graphics_passthrough: false,
        sixel_passthrough: false,
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
            Arc::new(AtomicUsize::new(0)),
            closing,
            Arc::new(AtomicU64::new(0)),
            live_input,
            false,
        )
        .await
    });

    let mut frame_bytes = [0_u8; 4096];
    let mut decoder = AttachFrameDecoder::new();
    while let Ok(Ok(bytes_read)) =
        tokio::time::timeout(Duration::from_millis(25), peer.read(&mut frame_bytes)).await
    {
        if bytes_read == 0 {
            break;
        }
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while decoder
            .next_message()
            .expect("decode initial attach frame")
            .is_some()
        {}
    }

    let overlay_marker = b"\x1b[s\x1b[?25l";
    let overlay_frame =
        OverlayFrame::new(b"\x1b[s\x1b[?25lDISPLAY-PANES\x1b[0m\x1b[u".to_vec(), 0, 1);
    test_control_tx
        .send(AttachControl::Overlay(overlay_frame))
        .expect("send overlay control");
    let mut collected = Vec::new();
    let overlay_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = overlay_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let read_timeout = remaining.min(Duration::from_millis(250));
        let bytes_read = match tokio::time::timeout(read_timeout, peer.read(&mut frame_bytes)).await
        {
            Ok(Ok(0)) => break,
            Ok(Ok(bytes_read)) => bytes_read,
            Ok(Err(error)) => panic!("read attach frame: {error}"),
            Err(_) => continue,
        };
        decoder.push_bytes(&frame_bytes[..bytes_read]);
        while let Some(message) = decoder.next_message().expect("decode attach frame") {
            match message {
                AttachMessage::Data(bytes) | AttachMessage::Render(bytes) => {
                    collected.extend_from_slice(&bytes)
                }
                _ => {}
            }
        }
        if collected
            .windows(overlay_marker.len())
            .any(|window| window == overlay_marker)
        {
            break;
        }
    }

    assert!(
        collected
            .windows(overlay_marker.len())
            .any(|window| window == overlay_marker),
        "overlay control should emit a frame, got: {:?}",
        String::from_utf8_lossy(&collected)
    );

    peer.shutdown().await.expect("close client peer");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}
