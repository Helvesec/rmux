use super::*;

#[tokio::test]
async fn l05_1_explicit_detach_orders_layout_before_detached() {
    detach_order(false).await;
}

#[tokio::test]
async fn l05_1_transport_loss_internal_resize_and_exact_once() {
    detach_order(true).await;
}

async fn detach_order(transport_loss: bool) {
    let handler = RequestHandler::new();
    let session = session_name("detach-client-notify");
    create_session(&handler, session.clone(), INITIAL_SIZE).await;
    set_window_size_policy(&handler, &session, "largest").await;

    let detaching_pid = std::process::id();
    let surviving_pid = detaching_pid.saturating_add(1);
    let detaching_size = TerminalSize {
        cols: 101,
        rows: 41,
    };
    let surviving_size = TerminalSize { cols: 60, rows: 20 };
    let (attach_id, _attach_events) =
        register_attached_client(&handler, detaching_pid, &session, detaching_size).await;
    let (_control_id, mut control_events) =
        register_control_client_with_id(&handler, surviving_pid, &session).await;
    let response = handler
        .handle(Request::RefreshClient(Box::new(
            refresh_client_size_request(surviving_pid, surviving_size),
        )))
        .await;
    assert!(
        matches!(response, Response::RefreshClient(_)),
        "{response:?}"
    );
    assert_eq!(
        session_size(&handler, &session).await,
        attached_content_size(detaching_size)
    );
    let layout_prefix = format!(
        "%layout-change @{} ",
        active_window_id(&handler, &session).await
    );
    settle_control_notifications(&mut control_events).await;

    let mut lifecycle = handler.subscribe_lifecycle_events();
    if transport_loss {
        handler.finish_attach(detaching_pid, attach_id).await;
    } else {
        let response = handler
            .handle(Request::DetachClient(DetachClientRequest))
            .await;
        assert!(
            matches!(response, Response::DetachClient(_)),
            "{response:?}"
        );
    }
    // Repeating transport cleanup after either cause must not publish again.
    handler.finish_attach(detaching_pid, attach_id).await;
    let mut internal = Vec::new();
    while let Ok(event) = lifecycle.try_recv() {
        match event.event {
            rmux_core::LifecycleEvent::WindowResized { target } => {
                assert_eq!(target.session_name(), &session);
                assert_eq!(target.window_index(), 0);
                internal.push("resize");
            }
            rmux_core::LifecycleEvent::ClientDetached {
                session_name,
                client_name,
            } => {
                assert_eq!(session_name, session);
                assert_eq!(
                    client_name.as_deref(),
                    Some(crate::client_names::attached_client_name(detaching_pid).as_str())
                );
                internal.push("detach");
            }
            _ => {}
        }
    }
    let expected = if transport_loss {
        ["detach", "resize"]
    } else {
        ["resize", "detach"]
    };
    assert_eq!(
        internal, expected,
        "internal lifecycle order and multiplicity"
    );

    assert_eq!(
        session_size(&handler, &session).await,
        surviving_size,
        "the session must fall back to its surviving control client's geometry"
    );
    let mut lines =
        collect_control_notifications_through(&mut control_events, "%client-detached ").await;
    lines.extend(
        tokio::time::timeout(
            CONTROL_NOTIFICATION_TIMEOUT,
            settle_control_notifications(&mut control_events),
        )
        .await
        .expect("bounded post-detach notification drain"),
    );
    let layout_index = lines
        .iter()
        .position(|line| line.starts_with(&layout_prefix))
        .expect("the surviving control client must be told the window shrank");
    assert_eq!(
        layout_change_geometry(&lines[layout_index]).as_deref(),
        Some("60x20"),
        "{:?}",
        lines[layout_index]
    );
    let detached: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("%client-detached "))
        .map(|(index, _)| index)
        .collect();
    let layouts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with(&layout_prefix))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        detached.len(),
        1,
        "one departure in the bounded operation trace: {lines:?}"
    );
    assert_eq!(
        layouts.len(),
        1,
        "one target-window layout in the bounded operation trace: {lines:?}"
    );
    assert_eq!(
        layouts[0] < detached[0],
        !transport_loss,
        "cause-specific control order: {lines:?}"
    );
}
