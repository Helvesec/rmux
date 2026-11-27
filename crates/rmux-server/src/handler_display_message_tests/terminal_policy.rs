use super::*;

#[tokio::test]
async fn ignored_opaque_byte_chunks_preserve_the_pending_palette_query() {
    let handler = RequestHandler::new();
    let session = session_name("ignored-opaque-palette");
    let pid = 46_130;
    let (tx, mut rx) = mpsc::unbounded_channel();
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 20, rows: 4 }),
                environment: None,
            }))
            .await,
    );
    handler.register_attach(pid, session.clone(), tx).await;
    let target = PaneTarget::new(session.clone(), 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    {
        let mut state = handler.state.lock().await;
        state
            .append_bytes_to_pane_transcript_for_test(&session, 0, 0, b"\x1b]4;7;?\x1b\\")
            .expect("pane palette query");
        state.start_pane_input_capture_for_test(&target);
    }
    assert!(matches!(
        handler
            .handle(Request::DisplayMessageExt(Box::new(
                DisplayMessageExtRequest {
                    target: Some(Target::Session(session)),
                    print: false,
                    message: Some("ignore opaque input".to_owned()),
                    target_client: Some(pid.to_string()),
                    empty_target_context: false,
                    duration_ms: Some(rmux_proto::DisplayMessageDurationMillis::new(10_000)),
                    ignore_input: true,
                }
            )))
            .await,
        Response::DisplayMessage(_)
    ));
    let _overlay = recv_overlay_control(&mut rx).await;
    let mut pending = Vec::new();
    for byte in b"\x1bPopaque\x1b]4;7;rgb:aaaa/bbbb/cccc\x07tail\x1b\\" {
        handler
            .handle_attached_live_input(pid, &mut pending, &[*byte])
            .await
            .unwrap();
    }
    assert!(pending.is_empty());
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .unwrap(),
        b""
    );

    let response = b"\x1b]4;7;rgb:1111/2222/3333\x1b\\";
    for byte in response {
        handler
            .handle_attached_live_input(pid, &mut pending, &[*byte])
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "the ignored-message decoder owns fragments"
        );
    }
    assert_eq!(
        handler
            .attached_input_capture_for_test(&target)
            .await
            .unwrap(),
        response
    );
}
