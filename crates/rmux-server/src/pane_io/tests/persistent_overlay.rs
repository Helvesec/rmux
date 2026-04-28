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
        closing,
        Arc::new(AtomicU64::new(0)),
        live_input,
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
