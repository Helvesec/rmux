use super::*;

#[tokio::test]
async fn floating_scrollbar_preconfigured_spawn_matches_format_and_native_size() {
    use rmux_core::formats::{render_template, FormatContext};
    use rmux_proto::{ProcessCommand, TerminalSize, WindowTarget};

    let mut state = HandlerState::default();
    let name = rmux_proto::SessionName::new("floating-scrollbar-spawn").unwrap();
    state
        .sessions
        .create_session(name.clone(), TerminalSize::new(80, 24))
        .unwrap();
    let command = ProcessCommand::Argv(vec!["/bin/cat".to_owned()]);
    let socket = std::path::Path::new("floating-scrollbar-spawn.sock");
    state
        .insert_initial_session_terminal(
            &name,
            crate::pane_terminals::InitialPaneSpawnOptions {
                socket_path: socket,
                spawn_environment: None,
                raw_spawn_environment: None,
                environment_overrides: None,
                command: Some(&command),
                pane_alert_callback: None,
                pane_exit_callback: None,
            },
        )
        .unwrap();
    for (option, value) in [
        (OptionName::PaneScrollbars, "on"),
        (OptionName::PaneScrollbarsPosition, "left"),
        (OptionName::PaneScrollbarsStyle, "width=2,pad=1"),
    ] {
        state
            .options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(name.clone(), 0)),
                option,
                value.to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    let created = state
        .split_window(
            SplitWindowTarget::Pane(PaneTarget::new(name.clone(), 0)),
            SplitDirection::Vertical,
            false,
            socket,
            None,
            None,
            SplitPaneProcess::Process(Some(&command)),
            None,
            None,
            None,
            false,
            true,
            Some(rmux_core::FloatingPaneRequest::new(Some(3), Some(4), 30, 8).unwrap()),
            None,
            None,
        )
        .unwrap();
    let session = state.sessions.session(&name).unwrap();
    let window = session.window();
    let pane = window.pane(created.pane.pane_index()).unwrap();
    let context = crate::format_runtime::RuntimeFormatContext::new(FormatContext::new())
        .with_session(session)
        .with_window(0, window)
        .with_pane(pane)
        .with_options(&state.options);
    let formatted = render_template("#{pane_width}|#{pane_height}", &context);
    let master = state
        .terminals
        .clone_pane_master(&name, pane.id(), 0, pane.index())
        .unwrap();
    let native = master.size().unwrap();
    state.shutdown_terminals_for_test();
    // tmux 3.7b reserves the three scrollbar columns at initial spawn.
    // No option mutation, refresh or resize is allowed after creation here.
    assert_eq!(formatted, "27|8");
    assert_eq!(native, TerminalSize::new(27, 8));
}

#[tokio::test]
async fn fnp1_installation_rejection_after_pty_spawn_restores_canonical_window() {
    let mut state = HandlerState::default();
    let session = rmux_proto::SessionName::new("fnp1-install").unwrap();
    state
        .sessions
        .create_session(
            session.clone(),
            rmux_proto::TerminalSize { cols: 80, rows: 24 },
        )
        .unwrap();
    // Deliberately omit the terminal-store owner, not the canonical target.
    // insert_pane must fail after open_pane_terminal has returned a real PTY.
    let command = rmux_proto::ProcessCommand::Argv(vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "read value".to_owned(),
    ]);
    let before = state.sessions.session(&session).unwrap().window().clone();
    let socket = std::env::temp_dir().join("fnp1-install.sock");
    let error = state
        .split_window(
            SplitWindowTarget::Pane(PaneTarget::new(session.clone(), 0)),
            SplitDirection::Vertical,
            false,
            &socket,
            None,
            None,
            SplitPaneProcess::Process(Some(&command)),
            None,
            None,
            None,
            false,
            false,
            Some(
                rmux_core::FloatingPaneRect::new(3, 2, 30, 8)
                    .unwrap()
                    .into(),
            ),
            None,
            None,
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("missing pane terminals"),
        "{error}"
    );
    let after = state.sessions.session(&session).unwrap().window();
    assert_eq!(after.panes().len(), before.panes().len());
    assert_eq!(after.active_pane_index(), before.active_pane_index());
    assert_eq!(after.last_pane_index(), before.last_pane_index());
    assert_eq!(after.floating_stack(), before.floating_stack());
    assert_eq!(
        after.pane(0).unwrap().geometry(),
        before.pane(0).unwrap().geometry()
    );
    assert!(state.pane_outputs.is_empty());
    assert!(state.transcripts.is_empty());
}
