use super::*;

#[test]
fn terminal_candidate_keeps_its_raw_height_when_status_consumes_every_row() {
    let size = TerminalSize { cols: 80, rows: 1 };
    let selected = selected_client_size(
        AttachedWindowSizePolicy::Latest,
        vec![AttachedSizeCandidate::from_terminal(size, 1, Some("3"))],
        &[],
        rmux_core::ManualWindowSize::new(size),
    );

    assert_eq!(
        selected,
        Some(SelectedWindowSize {
            constrained_manual: false,
            terminal_size: size,
            content_size: TerminalSize { cols: 80, rows: 0 },
        })
    );
}

#[test]
fn resizing_an_inactive_window_does_not_replace_the_active_terminal_geometry() {
    let mut session = crate::session_fixture::SessionFixture::new(
        SessionName::new("inactive-resize").expect("valid session name"),
        TerminalSize { cols: 80, rows: 24 },
    );
    let (inactive_window, _) = session
        .create_window(TerminalSize { cols: 90, rows: 30 })
        .expect("inactive window is created");
    let selected = SelectedWindowSize {
        constrained_manual: false,
        terminal_size: TerminalSize {
            cols: 120,
            rows: 40,
        },
        content_size: TerminalSize {
            cols: 120,
            rows: 39,
        },
    };

    selected.apply_to_window_switch(
        &mut session
            .store
            .window_switch_mut(&WindowTarget::with_window(
                session.name().clone(),
                inactive_window,
            ))
            .expect("inactive content resize succeeds"),
    );

    assert_eq!(session.terminal_size(), TerminalSize { cols: 80, rows: 24 });
    assert_eq!(
        session
            .window_at(inactive_window)
            .expect("inactive window survives")
            .size(),
        selected.content_size
    );
}
