use super::*;

#[tokio::test]
async fn recycled_selection_cannot_cross_validated_web_delivery() {
    for keyframe in [false, true] {
        let handler = Arc::new(RequestHandler::new());
        let session_name = create_quiet_session(&handler, "web-selection-delivery-race").await;
        let add_window = || {
            Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name.clone(),
                name: Some("selected".to_owned()),
                detached: true,
                environment: None,
                command: None,
                process_command: Some(rmux_proto::ProcessCommand::Argv(quiet_pane_command())),
                start_directory: None,
                target_window_index: Some(1),
                insert_at_target: false,
            }))
        };
        assert!(matches!(
            handler.handle(add_window()).await,
            Response::NewWindow(_)
        ));
        let selected_id = handler.web_window_id_for_test(&session_name, 1).await;
        let created = create_share(
            &handler,
            share_request(WebShareScope::Session(session_name.clone())),
        )
        .await;
        let mut client = TestWebSocket::connect(
            Arc::clone(&handler),
            &token_from_url(created.spectator_url.as_deref().expect("spectator URL")),
        )
        .await;
        assert_eq!(client.read_json().await["role"], "spectator");
        client
            .read_binary_with_prefix(0x10, "initial keyframe")
            .await;
        client
            .read_binary_with_prefix(0x01, "initial attach output")
            .await;
        client
            .send_json(r#"{"type":"select_window","window_index":1}"#)
            .await;
        let mut presentation = [false; 3];
        for _ in 0..MAX_INTERLEAVED_WEBSOCKET_FRAMES {
            let payload = client
                .read_binary_payload("valid selected presentation")
                .await;
            match payload.first() {
                Some(0x01) => presentation[0] = true,
                Some(0x10) => presentation[1] = true,
                Some(0x11) => {
                    presentation[2] |=
                        active_window_index(&parse_session_view(&payload)) == Some(1);
                }
                _ => {}
            }
            if presentation.into_iter().all(|received| received) {
                break;
            }
        }
        assert!(presentation.into_iter().all(|received| received));

        let boundary = super::super::selection_delivery_pause::install(&created.share_id, keyframe);
        if keyframe {
            // An explicit valid selection requests a fresh keyframe without
            // changing the server's active window or the selected WindowId.
            client
                .send_json(r#"{"type":"select_window","window_index":1}"#)
                .await;
        } else {
            handler
                .publish_web_pane_bytes_for_test(
                    &PaneTarget::with_window(session_name.clone(), 1, 0).into(),
                    b"delivery-boundary".to_vec(),
                )
                .await
                .expect("publish selected pane output");
        }
        timeout(Duration::from_secs(2), boundary.reached.notified())
            .await
            .expect("validated output reaches the deterministic delivery boundary");
        assert!(matches!(
            handler
                .handle(Request::KillWindow(KillWindowRequest {
                    target: WindowTarget::with_window(session_name.clone(), 1),
                    kill_all_others: false,
                }))
                .await,
            Response::KillWindow(_)
        ));
        assert!(matches!(
            handler.handle(add_window()).await,
            Response::NewWindow(_)
        ));
        assert_ne!(
            handler.web_window_id_for_test(&session_name, 1).await,
            selected_id
        );
        boundary.release.notify_one();

        timeout(Duration::from_secs(2), async {
            loop {
                match read_server_frame_inner(&mut client.stream).await {
                    Ok(frame) if frame.opcode == OPCODE_BINARY => {
                        if let Message::Binary(payload) =
                            client.opener.open(&frame.payload).expect("open frame")
                        {
                            assert!(
                                !matches!(payload.first(), Some(0x01 | 0x10)),
                                "recycled selected WindowId delivered terminal opcode {:?}",
                                payload.first()
                            );
                        }
                    }
                    Ok(frame) if frame.opcode == OPCODE_CLOSE => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::UnexpectedEof
                                | io::ErrorKind::ConnectionReset
                                | io::ErrorKind::ConnectionAborted
                                | io::ErrorKind::BrokenPipe
                        ) =>
                    {
                        break
                    }
                    Err(error) => panic!("unexpected close: {error}"),
                }
            }
        })
        .await
        .expect("stale selection fails closed");
        let _ = client.task.await.expect("server task joins");
        let _ = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: session_name,
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
    }
}
