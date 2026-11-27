use super::*;

async fn assert_control_keys_do_not_navigate(label: &str, next: &[u8], previous: &[u8]) {
    let handler = RequestHandler::new();
    let session = session_name(label);
    let requester_pid = std::process::id();
    let mut control_rx = create_quiet_attached_session(&handler, &session, requester_pid).await;
    run_overlay_command(
        &handler,
        requester_pid,
        r#"display-menu First 1 { display-message first } Second 2 { display-message second } Third 3 { display-message third }"#,
    )
    .await;
    let _ = next_overlay_frame(&mut control_rx).await;

    // tmux 3.7b ignores C-n/C-p in menus for all three terminal encodings.
    // Check both the initial selection and an interior item reached by Down.
    for (input, expected) in [
        (next, 0),
        (previous, 0),
        (b"\x1b[B".as_slice(), 1),
        (next, 1),
        (previous, 1),
        (b"\x1b[A".as_slice(), 0),
        (b"k".as_slice(), 2),
        (b"j".as_slice(), 0),
    ] {
        handler
            .handle_attached_live_input_for_test(requester_pid, input)
            .await
            .expect("menu input");
        let active_attach = handler.active_attach.lock().await;
        let active = &active_attach.by_pid[&requester_pid];
        let Some(ClientOverlayState::Menu(menu)) = active.overlay.as_ref() else {
            panic!("{label}: menu closed after {input:?}");
        };
        assert_eq!(menu.choice, Some(expected), "{label}: input {input:?}");
    }
    handler
        .handle_attached_live_input_for_test(requester_pid, b"3")
        .await
        .expect("explicit menu shortcut");
    assert!(handler.active_attach.lock().await.by_pid[&requester_pid]
        .overlay
        .is_none());
}

#[tokio::test]
async fn menu_control_navigation_c0_matches_tmux() {
    assert_control_keys_do_not_navigate("menu-c0-control", b"\x0e", b"\x10").await;
}

#[tokio::test]
async fn menu_control_navigation_modify_other_keys_matches_tmux() {
    assert_control_keys_do_not_navigate("menu-mok-control", b"\x1b[27;5;110~", b"\x1b[27;5;112~")
        .await;
}

#[tokio::test]
async fn menu_control_navigation_csi_u_matches_tmux() {
    assert_control_keys_do_not_navigate("menu-csiu-control", b"\x1b[110;5u", b"\x1b[112;5u").await;
}
