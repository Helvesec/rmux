use super::*;

#[tokio::test]
async fn attached_session_mutations_emit_refresh_switches() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha.clone(), control_tx)
        .await;

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
    let split_frame = take_render_frame(control_rx.try_recv().expect("split refresh"));
    assert!(split_frame.contains('│'));

    let resized = handler
        .handle(Request::ResizePane(rmux_proto::ResizePaneRequest {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        }))
        .await;
    assert_eq!(
        resized,
        Response::ResizePane(rmux_proto::ResizePaneResponse {
            target: PaneTarget::new(alpha.clone(), 0),
            adjustment: ResizePaneAdjustment::AbsoluteWidth { columns: 34 },
        })
    );
    let resize_frame = take_render_frame(control_rx.try_recv().expect("resize refresh"));
    assert!(resize_frame.contains('│'));

    let selected_layout = handler
        .handle(Request::SelectLayout(SelectLayoutRequest {
            target: SelectLayoutTarget::Session(alpha.clone()),
            layout: LayoutName::MainVertical,
        }))
        .await;
    assert_eq!(
        selected_layout,
        Response::SelectLayout(rmux_proto::SelectLayoutResponse {
            layout: LayoutName::MainVertical,
        })
    );
    let layout_frame = take_render_frame(control_rx.try_recv().expect("layout refresh"));
    assert!(layout_frame.contains('│'));

    let selected_pane = handler
        .handle(Request::SelectPane(SelectPaneRequest {
            target: PaneTarget::new(alpha, 1),
            title: None,
        }))
        .await;
    assert_eq!(
        selected_pane,
        Response::SelectPane(rmux_proto::SelectPaneResponse {
            target: PaneTarget::new(session_name("alpha"), 1),
        })
    );
    let select_frame = take_render_frame(control_rx.try_recv().expect("pane refresh"));
    assert!(select_frame.contains('│'));
    assert!(matches!(control_rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn switch_client_updates_the_tracked_session_for_follow_up_refreshes() {
    let handler = RequestHandler::new();
    let requester_pid = std::process::id();
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let split = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Session(beta.clone()),
            direction: rmux_proto::SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(requester_pid, alpha, control_tx)
        .await;

    let switched = handler
        .handle(Request::SwitchClient(SwitchClientRequest {
            target: beta.clone(),
        }))
        .await;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    let switch_frame = take_render_frame(control_rx.try_recv().expect("switch refresh"));
    assert!(switch_frame.contains('│'));

    let global_border = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::PaneActiveBorderStyle,
            value: "red".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(global_border, Response::SetOption(_)));
    let global_frame = take_render_frame(control_rx.try_recv().expect("global refresh"));
    assert!(global_frame.contains("\u{1b}[31m"));

    let beta_window = WindowTarget::with_window(beta.clone(), 0);
    let session_border = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(beta_window),
            option: OptionName::PaneActiveBorderStyle,
            value: "blue".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(session_border, Response::SetOption(_)));
    let session_frame = take_render_frame(control_rx.try_recv().expect("session refresh"));
    assert!(session_frame.contains("\u{1b}[34m"));

    let alpha_window = WindowTarget::with_window(session_name("alpha"), 0);
    let other_session = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Window(alpha_window),
            option: OptionName::PaneBorderStyle,
            value: "green".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(other_session, Response::SetOption(_)));
    assert!(matches!(control_rx.try_recv(), Err(TryRecvError::Empty)));
}

#[tokio::test]
async fn terminal_feature_mutations_refresh_attached_targets_with_client_context() {
    let handler = RequestHandler::new();
    let requester_pid = 42;
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        )
        .await;

    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            value: "xterm*:sync".to_owned(),
            mode: SetOptionMode::Append,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));

    let target = take_switch_target(control_rx.try_recv().expect("terminal feature refresh"));
    assert!(target.outer_terminal.features_string().contains("sync"));
    assert!(target
        .outer_terminal
        .wrap_render_frame(&target.render_frame)
        .starts_with(b"\x1b[?2026h"));
}

#[tokio::test]
async fn allow_passthrough_mutations_refresh_attached_targets() {
    let handler = RequestHandler::new();
    let requester_pid = 43;
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
        )
        .await;

    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::AllowPassthrough,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));

    let target = take_switch_target(control_rx.try_recv().expect("passthrough refresh"));
    assert!(
        target.kitty_graphics_passthrough,
        "allow-passthrough changes must recompute the attach target gate"
    );
    assert!(
        !target.sixel_passthrough,
        "kitty-only terminals must not enable sixel passthrough"
    );
}

#[tokio::test]
async fn allow_passthrough_enables_sixel_for_sixel_terminals() {
    let handler = RequestHandler::new();
    let requester_pid = 43;
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "foot")]),
        )
        .await;

    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::AllowPassthrough,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));

    let target = take_switch_target(control_rx.try_recv().expect("passthrough refresh"));
    assert!(
        target.sixel_passthrough,
        "allow-passthrough should enable sixel passthrough on sixel terminals"
    );
}

#[tokio::test]
async fn allow_passthrough_all_uses_active_pane_gate_for_now() {
    let handler = RequestHandler::new();
    let requester_pid = 43;
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha,
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
        )
        .await;

    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::AllowPassthrough,
            value: "all".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));

    let target = take_switch_target(control_rx.try_recv().expect("passthrough refresh"));
    assert!(
        target.kitty_graphics_passthrough,
        "all is accepted and currently shares the active-pane passthrough path"
    );
}

#[tokio::test]
async fn kitty_passthrough_is_disabled_while_active_pane_is_in_copy_mode() {
    let handler = RequestHandler::new();
    let requester_pid = 43;
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

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach_with_terminal_context(
            requester_pid,
            alpha.clone(),
            control_tx,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
        )
        .await;

    let set = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::AllowPassthrough,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(set, Response::SetOption(_)));
    let target = take_switch_target(control_rx.try_recv().expect("passthrough refresh"));
    assert!(
        target.kitty_graphics_passthrough,
        "kitty passthrough should be available before modal pane modes"
    );

    let copied = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(PaneTarget::new(alpha, 0)),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: false,
        }))
        .await;
    assert!(matches!(copied, Response::CopyMode(_)));

    let target = take_switch_target(control_rx.try_recv().expect("copy-mode refresh"));
    assert!(
        !target.kitty_graphics_passthrough,
        "modal pane modes must suppress live kitty passthrough"
    );
}

#[tokio::test]
async fn different_requester_pids_can_control_the_sole_active_attach() {
    let handler = RequestHandler::new();
    let owner_pid = 101;
    let intruder_pid = 202;
    let alpha = session_name("alpha");
    let beta = session_name("beta");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (control_tx, mut control_rx) = mpsc::unbounded_channel();
    let _attach_id = handler
        .register_attach(owner_pid, alpha.clone(), control_tx)
        .await;

    let switched = handler
        .dispatch(
            intruder_pid,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::SwitchClient(rmux_proto::SwitchClientResponse {
            session_name: beta.clone(),
        })
    );
    assert!(matches!(
        control_rx.try_recv(),
        Ok(AttachControl::Switch(_))
    ));

    let detached = handler
        .dispatch(
            intruder_pid,
            Request::DetachClient(rmux_proto::DetachClientRequest),
        )
        .await
        .response;
    assert_eq!(
        detached,
        Response::DetachClient(rmux_proto::DetachClientResponse)
    );
    assert!(matches!(control_rx.try_recv(), Ok(AttachControl::Detach)));
}

#[tokio::test]
async fn rename_session_preserves_ambiguity_rules_for_switch_and_detach() {
    let handler = RequestHandler::new();
    let alpha = session_name("alpha");
    let beta = session_name("beta");
    let gamma = session_name("gamma");

    for session_name in [alpha.clone(), beta.clone()] {
        let created = handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name,
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await;
        assert!(matches!(created, Response::NewSession(_)));
    }

    let (first_tx, _first_rx) = mpsc::unbounded_channel();
    let (second_tx, _second_rx) = mpsc::unbounded_channel();
    let _first_attach = handler.register_attach(101, alpha.clone(), first_tx).await;
    let _second_attach = handler.register_attach(202, alpha.clone(), second_tx).await;

    let renamed = handler
        .handle(Request::RenameSession(RenameSessionRequest {
            target: alpha,
            new_name: gamma,
        }))
        .await;
    assert!(matches!(renamed, Response::RenameSession(_)));

    let switched = handler
        .dispatch(
            303,
            Request::SwitchClient(SwitchClientRequest {
                target: beta.clone(),
            }),
        )
        .await
        .response;
    assert_eq!(
        switched,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(
                "switch-client requires an unambiguous attached client".to_owned(),
            ),
        })
    );

    let detached = handler
        .dispatch(303, Request::DetachClient(DetachClientRequest))
        .await
        .response;
    assert_eq!(
        detached,
        Response::Error(ErrorResponse {
            error: RmuxError::Server(
                "detach-client requires an unambiguous attached client".to_owned(),
            ),
        })
    );
}
