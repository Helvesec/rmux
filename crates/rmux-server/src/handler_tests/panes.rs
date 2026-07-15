use super::*;

#[tokio::test]
async fn split_window_routes_session_and_pane_targets_to_the_expected_panes() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let first_split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        first_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 1),
        })
    );

    let selected = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: PaneTarget::new(alpha.clone(), 1),
            title: None,
            style: None,
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;
    assert_eq!(
        selected,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: PaneTarget::new(alpha.clone(), 1),
        })
    );

    let active_split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        active_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 2),
        })
    );

    let explicit_split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(PaneTarget::new(alpha.clone(), 0)),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        explicit_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");

    assert_eq!(session.active_pane_index(), 1);
    assert_eq!(
        session
            .window()
            .panes()
            .iter()
            .map(|pane| pane.index())
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
}

#[tokio::test]
async fn target_action_split_and_resize_resolve_raw_targets_server_side() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let split = handler
        .handle(Request::SplitWindowTargetAction(Box::new(
            SplitWindowTargetActionRequest {
                target: Some("alpha:0.0".to_owned()),
                direction: rmux_proto::SplitDirection::Vertical,
                before: false,
                environment: None,
                command: None,
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    assert_eq!(
        split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 1),
        })
    );

    let resized = handler
        .handle(Request::ResizePaneTargetAction(
            ResizePaneTargetActionRequest {
                target: Some("alpha:0.1".to_owned()),
                adjustment: ResizePaneAdjustment::Right { cells: 3 },
            },
        ))
        .await;
    assert_eq!(
        resized,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(alpha, 1),
            adjustment: ResizePaneAdjustment::Right { cells: 3 },
        })
    );
}

#[tokio::test]
async fn sdk_split_identity_returns_visible_base_index_and_stable_id_atomically() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::PaneBaseIndex,
                value: "5".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let response = handler
        .handle(Request::SplitWindowIdentity(Box::new(
            SplitWindowIdentityRequest {
                action: SplitWindowTargetActionRequest {
                    target: Some("alpha:0.5".to_owned()),
                    direction: rmux_proto::SplitDirection::Vertical,
                    before: false,
                    environment: None,
                    command: None,
                    process_command: None,
                    start_directory: None,
                    keep_alive_on_exit: None,
                    detached: false,
                    size: None,
                    preserve_zoom: false,
                    full_size: false,
                    stdin_payload: None,
                },
            },
        )))
        .await;

    let Response::SplitWindowIdentity(identity) = response else {
        panic!("expected atomic split identity response, got {response:?}");
    };
    assert_eq!(identity.pane, PaneTarget::new(alpha.clone(), 6));

    let state = handler.state.lock().await;
    let pane_id = state
        .sessions
        .session(&alpha)
        .expect("session exists")
        .window()
        .pane(1)
        .expect("new raw pane index exists")
        .id();
    assert_eq!(identity.pane_id, pane_id);
}

#[tokio::test]
async fn select_pane_style_sets_pane_style_and_format_colours() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let target = PaneTarget::new(alpha.clone(), 1);

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 10 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
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

    let response = handler
        .handle(Request::SelectPane(Box::new(SelectPaneRequest {
            target: target.clone(),
            title: None,
            style: Some("fg=blue,bg=red".to_owned()),
            input_disabled: None,
            preserve_zoom: false,
        })))
        .await;

    assert_eq!(
        response,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: target.clone(),
        })
    );
    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(session.active_pane_index(), 1);
        assert_eq!(
            state.options.pane_value(&target, OptionName::WindowStyle),
            Some("fg=blue,bg=red")
        );
    }

    let response = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(target)),
            print: true,
            message: Some("#{pane_active}:#{pane_fg}:#{pane_bg}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let Response::DisplayMessage(response) = response else {
        panic!("expected display-message response");
    };
    let output = response
        .command_output()
        .expect("display-message -p returns output");
    assert_eq!(output.stdout(), b"1:blue:red\n");
}

#[tokio::test]
async fn split_window_rolls_back_the_session_when_terminal_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let failed_split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        failed_split,
        Response::Error(ErrorResponse {
            error: RmuxError::Server("injected pane terminal resize failure".to_owned()),
        })
    );

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(session.window().panes().len(), 1);
        assert_eq!(
            session.window().pane(0).expect("pane 0 exists").geometry(),
            PaneGeometry::new(0, 0, 120, 40)
        );
    }

    let retried_split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        retried_split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha, 1),
        })
    );
}

#[tokio::test]
async fn horizontal_split_updates_layout_and_geometry() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 100,
                rows: 50,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 1),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.window().layout(), LayoutName::MainVertical);
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 50, 50)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(51, 0, 49, 50)
    );
}

#[tokio::test]
async fn kill_pane_removes_the_terminal_and_uses_last_pane_fallback() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

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
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::new(alpha.clone(), 1),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SelectPane(Box::new(SelectPaneRequest {
                target: PaneTarget::new(alpha.clone(), 0),
                title: None,
                style: None,
                input_disabled: None,
                preserve_zoom: false,
            })))
            .await,
        Response::SelectPane(_)
    ));

    let (removed_pane_id, surviving_pane_id) = {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        (
            session.window().pane(0).expect("pane 0 exists").id(),
            session.window().pane(1).expect("pane 1 exists").id(),
        )
    };
    let now = std::time::Instant::now();
    assert_eq!(
        handler.observe_pane_snapshot_revision(removed_pane_id, 1, now),
        Some(1)
    );
    assert_eq!(
        handler.observe_pane_snapshot_revision(surviving_pane_id, 7, now),
        Some(7)
    );

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            window_destroyed: false,
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(session.active_pane_index(), 0);
    assert_eq!(session.window().last_pane_index(), None);
    assert_eq!(
        session.window().pane(0).map(|pane| pane.id()),
        Some(surviving_pane_id)
    );
    state
        .ensure_panes_exist(&alpha, &[surviving_pane_id])
        .expect("surviving pane terminal should remain");
    assert_eq!(
        state.ensure_panes_exist(&alpha, &[removed_pane_id]),
        Err(RmuxError::Server(format!(
            "missing pane terminal for pane id {} in session {}",
            removed_pane_id.as_u32(),
            alpha
        )))
    );
    drop(state);
    assert_eq!(
        handler.last_emitted_pane_snapshot_revision(removed_pane_id),
        None
    );
    assert_eq!(
        handler.last_emitted_pane_snapshot_revision(surviving_pane_id),
        Some(7)
    );
}

#[tokio::test]
async fn kill_pane_rolls_back_when_terminal_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 120,
                rows: 40,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

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

    let removed_pane_id = {
        let state = handler.state.lock().await;
        state
            .sessions
            .session(&alpha)
            .expect("session exists")
            .window()
            .pane(1)
            .expect("pane 1 exists")
            .id()
    };

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 1),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::Error(ErrorResponse {
            error: RmuxError::Server("injected pane terminal resize failure".to_owned()),
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert!(session.window().pane(1).is_some());
    state
        .ensure_panes_exist(&alpha, &[removed_pane_id])
        .expect("rolled back pane terminal should be restored");
}

#[tokio::test]
async fn kill_last_pane_in_only_window_removes_the_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40
                }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));

    let killed = handler
        .handle(Request::KillPane(KillPaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            kill_all_except: false,
        }))
        .await;
    assert_eq!(
        killed,
        Response::KillPane(rmux_proto::KillPaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            window_destroyed: true,
        })
    );

    let state = handler.state.lock().await;
    assert!(
        state.sessions.session(&alpha).is_none(),
        "killing the final pane must remove the session"
    );
}

#[tokio::test]
async fn resize_pane_rolls_back_geometry_when_terminal_resize_fails() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: Some(TerminalSize {
                cols: 200,
                rows: 50,
            }),

            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(alpha.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,

            environment: None,
        }))
        .await;
    assert_eq!(
        split,
        Response::SplitWindow(rmux_proto::SplitWindowResponse {
            pane: PaneTarget::new(alpha.clone(), 1),
        })
    );

    {
        let mut state = handler.state.lock().await;
        state.fail_next_resize_for_test();
    }

    let failed_resize = handler
        .handle(Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await;
    assert_eq!(
        failed_resize,
        Response::Error(ErrorResponse {
            error: RmuxError::Server("injected pane terminal resize failure".to_owned()),
        })
    );

    {
        let state = handler.state.lock().await;
        let session = state.sessions.session(&alpha).expect("session exists");
        assert_eq!(
            session.window().pane(0).expect("pane 0 exists").geometry(),
            PaneGeometry::new(0, 0, 100, 50)
        );
        assert_eq!(
            session.window().pane(1).expect("pane 1 exists").geometry(),
            PaneGeometry::new(101, 0, 99, 50)
        );
    }

    let retried_resize = handler
        .handle(Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await;
    assert_eq!(
        retried_resize,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        })
    );

    let state = handler.state.lock().await;
    let session = state.sessions.session(&alpha).expect("session exists");
    assert_eq!(
        session.window().pane(0).expect("pane 0 exists").geometry(),
        PaneGeometry::new(0, 0, 34, 50)
    );
    assert_eq!(
        session.window().pane(1).expect("pane 1 exists").geometry(),
        PaneGeometry::new(35, 0, 165, 50)
    );
}

#[tokio::test]
async fn resize_pane_noop_validates_target_slot() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");

    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: alpha.clone(),
            detached: true,
            size: None,
            environment: None,
        }))
        .await;
    assert!(matches!(created, Response::NewSession(_)));

    let missing_pane_resize = handler
        .handle(Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 9),
            adjustment: ResizePaneAdjustment::NoOp,
        }))
        .await;
    assert_eq!(
        missing_pane_resize,
        Response::Error(ErrorResponse {
            error: RmuxError::invalid_target("alpha:0.9", "pane index does not exist in session"),
        })
    );
}
