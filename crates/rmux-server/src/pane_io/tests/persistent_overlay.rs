use super::*;

#[tokio::test]
async fn forward_attach_clears_persistent_overlay_with_fresh_switch_frame() {
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
        test_attach_target(&session_name, b"BASE-OLD", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let initial = read_attach_data_until(&mut peer, b"BASE-OLD").await;
    assert!(
        String::from_utf8_lossy(&initial).contains("BASE-OLD"),
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
    let _ = read_attach_data_until(&mut peer, b"MENU-OLD").await;

    control_tx
        .send(AttachControl::AdvancePersistentOverlayState(8))
        .expect("send overlay state advance");
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            Vec::new(),
            0,
            2,
            8,
        )))
        .expect("send persistent overlay clear");
    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"BASE-FRESH",
            None,
        )))
        .expect("send refreshed attach target");

    let refresh = read_attach_data_until(&mut peer, b"BASE-FRESH").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        !refresh_text.contains("BASE-OLD"),
        "overlay teardown must not paint stale base before the fresh switch: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_does_not_paint_stale_base_while_overlay_dismiss_refresh_is_pending() {
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
        test_attach_target(&session_name, b"STALE-BASE", None),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _ = read_attach_data_until(&mut peer, b"STALE-BASE").await;
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU-OLD".to_vec(),
            0,
            1,
            7,
        )))
        .expect("send initial persistent overlay");
    let _ = read_attach_data_until(&mut peer, b"MENU-OLD").await;

    control_tx
        .send(AttachControl::AdvancePersistentOverlayState(8))
        .expect("send overlay state advance");
    let pending_bytes = read_attach_data_for(&mut peer, Duration::from_millis(100)).await;
    let pending_text = String::from_utf8_lossy(&pending_bytes);
    assert!(
        !pending_text.contains("STALE-BASE"),
        "state advance must wait for the fresh switch instead of repainting a stale base: {pending_text:?}"
    );

    control_tx
        .send(AttachControl::switch(test_attach_target(
            &session_name,
            b"FRESH-BASE",
            None,
        )))
        .expect("send refreshed attach target");
    let refresh = read_attach_data_until(&mut peer, b"FRESH-BASE").await;
    let refresh_text = String::from_utf8_lossy(&refresh);
    assert!(
        !refresh_text.contains("STALE-BASE"),
        "overlay teardown must be resolved by the fresh switch: {refresh_text:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_defers_kitty_passthroughs_until_persistent_overlay_clears() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
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
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU".to_vec(),
            0,
            1,
            7,
        )))
        .expect("send persistent overlay");
    let _ = read_attach_data_until(&mut peer, b"MENU").await;

    pane_output.send_for_generation_with_passthroughs(
        None,
        b"tick".to_vec(),
        vec![rmux_core::TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAA",
        )],
    );
    let pending = read_attach_data_for(&mut peer, Duration::from_millis(100)).await;
    assert!(
        !pending
            .windows(b"\x1b_G".len())
            .any(|window| window == b"\x1b_G"),
        "kitty passthrough should not be emitted while overlay is visible: {pending:?}"
    );

    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            Vec::new(),
            0,
            2,
            8,
        )))
        .expect("send persistent overlay clear");

    let rendered = read_attach_data_until(&mut peer, b"\x1b_Gf=100;AAA\x1b\\").await;
    assert!(
        rendered
            .windows(b"\x1b_Gf=100;AAA\x1b\\".len())
            .any(|window| window == b"\x1b_Gf=100;AAA\x1b\\"),
        "deferred kitty passthrough should flush after overlay clears: {rendered:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_defers_sixel_passthroughs_until_persistent_overlay_clears() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let pane_output = pane_output_channel();
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target_with_protocols(
            &session_name,
            b"BASE-0",
            None,
            pane_output.clone(),
            false,
            true,
        ),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;
    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            b"MENU".to_vec(),
            0,
            1,
            7,
        )))
        .expect("send persistent overlay");
    let _ = read_attach_data_until(&mut peer, b"MENU").await;

    pane_output.send_for_generation_with_passthroughs(
        None,
        b"tick".to_vec(),
        vec![rmux_core::TerminalPassthrough::sixel(0, 0, b"q#0!10~")],
    );
    let pending = read_attach_data_for(&mut peer, Duration::from_millis(100)).await;
    assert!(
        !pending
            .windows(b"\x1bPq#0!10~\x1b\\".len())
            .any(|window| window == b"\x1bPq#0!10~\x1b\\"),
        "sixel passthrough should not be emitted while overlay is visible: {pending:?}"
    );

    control_tx
        .send(AttachControl::Overlay(OverlayFrame::persistent_with_state(
            Vec::new(),
            0,
            2,
            8,
        )))
        .expect("send persistent overlay clear");

    let rendered = read_attach_data_until(&mut peer, b"\x1bPq#0!10~\x1b\\").await;
    assert!(
        rendered
            .windows(b"\x1bPq#0!10~\x1b\\".len())
            .any(|window| window == b"\x1bPq#0!10~\x1b\\"),
        "deferred sixel passthrough should flush after overlay clears: {rendered:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

#[tokio::test]
async fn forward_attach_drops_kitty_passthroughs_when_target_gate_is_disabled() {
    let handler = Arc::new(RequestHandler::new());
    let session_name = SessionName::new("alpha").expect("valid session name");
    let (stream, mut peer) = tokio::net::UnixStream::pair().expect("attach stream pair");
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (_control_tx, control_rx) = mpsc::unbounded_channel();
    let closing = Arc::new(AtomicBool::new(false));
    let pane_output = pane_output_channel();
    let live_input = LiveAttachInputContext {
        handler,
        attach_pid: std::process::id(),
    };

    let attach_task = tokio::spawn(forward_attach(
        stream,
        test_attach_target_with_output(&session_name, b"BASE-0", None, pane_output.clone(), false),
        Vec::new(),
        shutdown_rx,
        control_rx,
        Arc::new(AtomicUsize::new(0)),
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
        false,
    ));

    let _ = read_attach_data_until(&mut peer, b"BASE-0").await;
    pane_output.send_for_generation_with_passthroughs(
        None,
        b"tick".to_vec(),
        vec![rmux_core::TerminalPassthrough::kitty_graphics(
            0,
            0,
            b"Gf=100;AAA",
        )],
    );

    let pending = read_attach_data_for(&mut peer, Duration::from_millis(100)).await;
    assert!(
        !pending
            .windows(b"\x1b_G".len())
            .any(|window| window == b"\x1b_G"),
        "disabled kitty passthrough target should never emit ESC_G: {pending:?}"
    );

    shutdown_tx.send(()).expect("request attach shutdown");
    let result = attach_task.await.expect("attach task join");
    assert!(
        result.is_ok(),
        "forward_attach should stay healthy: {result:?}"
    );
}

async fn read_attach_data_for(peer: &mut tokio::net::UnixStream, duration: Duration) -> Vec<u8> {
    let mut collected = Vec::new();
    let mut frame_bytes = [0_u8; 4096];
    let mut decoder = AttachFrameDecoder::new();
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            result = peer.read(&mut frame_bytes) => {
                let bytes_read = result.expect("read peer bytes");
                if bytes_read == 0 {
                    break;
                }
                decoder.push_bytes(&frame_bytes[..bytes_read]);
                while let Some(message) = decoder.next_message().expect("decode attach frame") {
                    if let AttachMessage::Data(bytes) | AttachMessage::Render(bytes) = message {
                        collected.extend_from_slice(&bytes);
                    }
                }
            }
        }
    }

    collected
}
