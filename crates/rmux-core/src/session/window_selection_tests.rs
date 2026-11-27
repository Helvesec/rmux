use crate::{PaneId, Session};
use rmux_proto::{ResizePaneAdjustment, RmuxError, SessionName, SplitDirection, TerminalSize};

#[test]
fn standalone_and_clone_remain_owning_values() {
    for preserve in [false, true] {
        let mut original = Session::new(
            SessionName::new("standalone").unwrap(),
            TerminalSize { cols: 80, rows: 24 },
        );
        original
            .split_pane_in_window_with_id_and_direction_before(
                0,
                0,
                PaneId::new(99),
                SplitDirection::Vertical,
                true,
            )
            .unwrap();
        original.select_pane(1).unwrap();
        original.resize_pane(1, ResizePaneAdjustment::Zoom).unwrap();
        let mut cloned = original.clone();
        cloned
            .select_pane_in_window_with_zoom(0, 0, preserve)
            .unwrap();
        assert_eq!(original.window().active_pane_index(), 1);
        assert_eq!(cloned.window().active_pane_index(), 0);
        assert_eq!(cloned.window().last_pane_index(), Some(1));
        assert_eq!(cloned.window().is_zoomed(), preserve);
        assert_eq!(cloned.window().pane(0).unwrap().id(), PaneId::new(99));
        cloned.select_pane(0).unwrap();
        assert_eq!(cloned.window().last_pane_index(), Some(1));
        let before = cloned.clone();
        assert_eq!(
            cloned.select_pane_in_window(9, 99),
            Err(RmuxError::invalid_target(
                "standalone:9",
                "window index does not exist in session"
            ))
        );
        assert_eq!(cloned, before);
    }
}

fn detached_split_session(splits: u32) -> Session {
    let mut session = Session::new(
        SessionName::new("lastfallback").unwrap(),
        TerminalSize { cols: 80, rows: 24 },
    );
    for id in 1..=splits {
        session
            .split_pane_in_window_with_id_and_direction_before_detached(
                0,
                0,
                PaneId::new(id),
                SplitDirection::Vertical,
                false,
                true,
            )
            .unwrap();
    }
    session
}

#[test]
fn last_pane_in_two_pane_window_without_last_selects_the_other_pane_like_tmux() {
    // tmux 3.7b cmd-select-pane.c: with no last pane and exactly two panes,
    // last-pane / select-pane -l switch to the other pane (rc=0).
    for preserve in [false, true] {
        let mut session = detached_split_session(1);
        assert_eq!(session.window().last_pane_index(), None);
        assert_eq!(session.window().active_pane_index(), 0);
        assert_eq!(session.last_pane_in_window_with_zoom(0, preserve), Ok(1));
        assert_eq!(session.window().active_pane_index(), 1);
        assert_eq!(session.window().last_pane_index(), Some(0));
        assert_eq!(session.last_pane_in_window_with_zoom(0, preserve), Ok(0));
        assert_eq!(session.window().active_pane_index(), 0);
    }
}

#[test]
fn last_pane_without_last_still_fails_outside_two_pane_windows() {
    for splits in [0, 2] {
        let mut session = detached_split_session(splits);
        let before = session.clone();
        assert_eq!(
            session.last_pane_in_window(0),
            Err(RmuxError::Server("no last pane".to_owned()))
        );
        assert_eq!(session, before);
    }
}
