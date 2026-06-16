use super::*;

#[tokio::test]
async fn navigation_commands_wrap_and_remain_session_scoped() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    insert_window(&handler, &alpha, 3).await;
    insert_window(&handler, &alpha, 7).await;
    insert_window(&handler, &beta, 4).await;

    assert_eq!(
        handler
            .handle(Request::NextWindow(NextWindowRequest {
                target: alpha.clone(),
                alerts_only: false,
            }))
            .await,
        Response::NextWindow(rmux_proto::NextWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 3),
        })
    );
    assert_eq!(
        handler
            .handle(Request::NextWindow(NextWindowRequest {
                target: alpha.clone(),
                alerts_only: false,
            }))
            .await,
        Response::NextWindow(rmux_proto::NextWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 7),
        })
    );
    assert_eq!(
        handler
            .handle(Request::PreviousWindow(PreviousWindowRequest {
                target: alpha.clone(),
                alerts_only: false,
            }))
            .await,
        Response::PreviousWindow(rmux_proto::PreviousWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 3),
        })
    );
    assert_eq!(
        handler
            .handle(Request::LastWindow(LastWindowRequest {
                target: alpha.clone(),
            }))
            .await,
        Response::LastWindow(rmux_proto::LastWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 7),
        })
    );

    let state = handler.state.lock().await;
    let alpha_session = state
        .sessions
        .session(&alpha)
        .expect("alpha session should exist");
    let beta_session = state
        .sessions
        .session(&beta)
        .expect("beta session should exist");
    assert_eq!(alpha_session.active_window_index(), 7);
    assert_eq!(alpha_session.last_window_index(), Some(3));
    assert_eq!(beta_session.active_window_index(), 0);
    assert_eq!(beta_session.last_window_index(), None);
}

#[tokio::test]
async fn navigation_commands_return_tmux_style_errors_when_history_is_missing() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;

    assert_eq!(
        handler
            .handle(Request::LastWindow(LastWindowRequest {
                target: alpha.clone(),
            }))
            .await,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Message("no last window".to_owned()),
        })
    );

    assert_eq!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 2),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(rmux_proto::KillWindowResponse {
            target: WindowTarget::with_window(alpha.clone(), 0),
        })
    );

    assert_eq!(
        handler
            .handle(Request::NextWindow(NextWindowRequest {
                target: alpha,
                alerts_only: false,
            }))
            .await,
        Response::Error(rmux_proto::ErrorResponse {
            error: rmux_proto::RmuxError::Message("no next window".to_owned()),
        })
    );
}

#[tokio::test]
async fn list_windows_returns_structured_entries_and_rendered_stdout() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;
    insert_window(&handler, &alpha, 2).await;

    assert!(matches!(
        handler
            .handle(Request::RenameWindow(RenameWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 2),
                name: "logs".to_owned(),
            }))
            .await,
        Response::RenameWindow(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 2),
            }))
            .await,
        Response::SelectWindow(_)
    ));

    let response = handler
        .handle(Request::ListWindows(ListWindowsRequest {
            target: alpha.clone(),
            format: Some("#{window_index}:#{window_id}:#{window_last_flag}".to_owned()),
        }))
        .await;

    let Response::ListWindows(response) = response else {
        panic!("expected list-windows response");
    };
    assert_eq!(response.windows.len(), 2);
    assert_eq!(
        response.windows[0].target,
        WindowTarget::with_window(alpha.clone(), 0)
    );
    assert_eq!(response.windows[0].window_id, "@0");
    assert_eq!(response.windows[0].rendered, "0:@0:1");
    assert!(response.windows[0].last);
    assert!(!response.windows[0].active);
    assert_eq!(
        response.windows[1].target,
        WindowTarget::with_window(alpha.clone(), 2)
    );
    assert_eq!(response.windows[1].name.as_deref(), Some("logs"));
    assert_eq!(response.windows[1].window_id, "@1");
    assert_eq!(response.windows[1].rendered, "2:@1:0");
    assert!(response.windows[1].active);
    assert_eq!(
        std::str::from_utf8(response.output.stdout()).expect("list-windows output is utf-8"),
        "0:@0:1\n2:@1:0\n"
    );
}

#[tokio::test]
async fn list_windows_format_uses_each_windows_active_pane_context() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
            }))
            .await,
        Response::SplitWindow(_)
    ));
    insert_window(&handler, &alpha, 2).await;

    let expected_active_panes = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("alpha exists");
        session
            .windows()
            .iter()
            .map(|(window_index, window)| {
                format!("{}:{}", window_index, window.active_pane_index())
            })
            .collect::<Vec<_>>()
    };

    let response = handler
        .handle(Request::ListWindows(ListWindowsRequest {
            target: alpha.clone(),
            format: Some("#{window_index}:#{pane_index}".to_owned()),
        }))
        .await;

    let Response::ListWindows(response) = response else {
        panic!("expected list-windows response");
    };
    assert_eq!(
        response
            .windows
            .iter()
            .map(|window| window.rendered.clone())
            .collect::<Vec<_>>(),
        expected_active_panes
    );
}

#[tokio::test]
async fn window_mutations_refresh_attached_sessions() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    create_session(&handler, "alpha").await;

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;
    drain_attach_controls(&mut control_rx).await;

    let new_window = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: alpha.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: Some(quiet_window_test_command()),
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;
    assert!(
        matches!(new_window, Response::NewWindow(_)),
        "{new_window:?}"
    );
    assert_refresh(control_rx.try_recv().expect("new-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::SelectWindow(SelectWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
            }))
            .await,
        Response::SelectWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("select-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::NextWindow(NextWindowRequest {
                target: alpha.clone(),
                alerts_only: false,
            }))
            .await,
        Response::NextWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("next-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::PreviousWindow(PreviousWindowRequest {
                target: alpha.clone(),
                alerts_only: false,
            }))
            .await,
        Response::PreviousWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("previous-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::LastWindow(LastWindowRequest {
                target: alpha.clone(),
            }))
            .await,
        Response::LastWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("last-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::RenameWindow(RenameWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
                name: "logs".to_owned(),
            }))
            .await,
        Response::RenameWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("rename-window refresh"));

    assert!(matches!(
        handler
            .handle(Request::KillWindow(KillWindowRequest {
                target: WindowTarget::with_window(alpha.clone(), 1),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));
    assert_refresh(control_rx.try_recv().expect("kill-window refresh"));
    drain_attach_controls(&mut control_rx).await;

    assert!(matches!(
        handler
            .handle(Request::ListWindows(ListWindowsRequest {
                target: alpha,
                format: None,
            }))
            .await,
        Response::ListWindows(_)
    ));
    match timeout(Duration::from_millis(100), control_rx.recv()).await {
        Err(_) | Ok(None) => {}
        Ok(Some(control)) => {
            panic!("list-windows should not refresh attached clients, got {control:?}")
        }
    }
}
