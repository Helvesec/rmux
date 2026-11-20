use super::HandlerState;
use rmux_proto::{RmuxError, SessionName, TerminalSize, WindowTarget};

#[test]
fn no_terminal_restores_selection_before_compensation() {
    let mut state = HandlerState::window_selection_fixture(true);
    let b = SessionName::new("B").unwrap();
    let target = WindowTarget::with_window(b.clone(), 7);
    let before = state.window_selection_snapshot();
    let error = state
        .mutate_window_and_resize_terminal_with_family(&target, |selection| {
            selection.select(0, false)
        })
        .unwrap_err();
    assert_eq!(error, RmuxError::Server("failed to roll back window runtime for B:7 after server error: missing pane terminals for session A: server error: missing pane terminals for session A".into()));
    assert_eq!(state.window_runtime_resize_count_for_test(), 2);
    for (session, index) in [("A", 0), ("B", 7), ("C", 0)] {
        let name = SessionName::new(session).unwrap();
        let window = state
            .sessions
            .session(&name)
            .unwrap()
            .window_at(index)
            .unwrap();
        assert_eq!(
            window.active_pane_index(),
            1,
            "{session}:{index} restored selection"
        );
        assert_eq!(window.last_pane_index(), Some(0));
        assert!(window.is_zoomed());
    }
    assert_eq!(state.window_selection_snapshot(), before);
    let pending = state
        .take_applied_window_resizes()
        .into_iter()
        .map(|x| {
            let (target, prepared, _) = x.into_parts();
            (target, prepared)
        })
        .collect::<Vec<_>>();
    assert_eq!(pending, vec![(WindowTarget::with_window(b, 0), true)]);
}

#[test]
fn switch_window_failure_restores_selection_geometry_touch_and_linked_family() {
    let mut state = HandlerState::window_selection_fixture(true);
    let b = SessionName::new("B").unwrap();
    let target = WindowTarget::with_window(b.clone(), 7);
    let session_id = state.sessions.session(&b).unwrap().id();
    let before = state.window_selection_snapshot();
    let error = state
        .mutate_switch_window_and_resize_terminal_with_family(
            &target,
            Some(session_id),
            true,
            |mut switch| {
                switch.select_window_and_pane(0, true)?;
                switch.resize(
                    TerminalSize {
                        cols: 100,
                        rows: 31,
                    },
                    TerminalSize {
                        cols: 100,
                        rows: 30,
                    },
                );
                Ok(())
            },
        )
        .unwrap_err();

    assert!(
        error.to_string().contains("missing pane terminals"),
        "{error}"
    );
    assert_eq!(state.window_runtime_resize_count_for_test(), 2);
    assert_eq!(state.window_selection_snapshot(), before);
}

#[test]
fn legacy_no_resize_records_new_geometry_and_preserves_old_queue() {
    let mut state = HandlerState::window_selection_fixture(false);
    let b = SessionName::new("B").unwrap();
    let size = TerminalSize {
        cols: 100,
        rows: 30,
    };
    state
        .mutate_session_and_resize_window_terminal_with_family_if(
            &b,
            7,
            |session| session.resize_window(7, size),
            |_| false,
        )
        .unwrap();
    for (session, index) in [("A", 0), ("B", 7), ("C", 0)] {
        let name = SessionName::new(session).unwrap();
        assert_eq!(
            state
                .sessions
                .session(&name)
                .unwrap()
                .window_at(index)
                .unwrap()
                .size(),
            size
        );
    }
    assert_eq!(
        state
            .sessions
            .session(&b)
            .unwrap()
            .window_at(0)
            .unwrap()
            .size(),
        TerminalSize { cols: 80, rows: 24 }
    );
    assert_eq!(state.window_runtime_resize_count_for_test(), 0);
    let _ = state.window_selection_snapshot();
    let pending = state
        .take_applied_window_resizes()
        .into_iter()
        .map(|x| {
            let (target, prepared, _) = x.into_parts();
            (target, prepared)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pending,
        vec![
            (WindowTarget::with_window(b.clone(), 0), true),
            (WindowTarget::with_window(b, 7), false)
        ]
    );
}
