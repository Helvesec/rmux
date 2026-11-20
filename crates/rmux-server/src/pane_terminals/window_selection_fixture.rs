use super::HandlerState;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use rmux_core::ManagedSessionTestSnapshot;
use rmux_proto::*;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct WindowSelectionSnapshot {
    sessions: Vec<ManagedSessionTestSnapshot>,
    ancillary: Vec<String>,
}

impl HandlerState {
    pub(crate) fn window_selection_fixture(zoom: bool) -> Self {
        let mut state = Self::default();
        let a = SessionName::new("A").unwrap();
        let b = SessionName::new("B").unwrap();
        let c = SessionName::new("C").unwrap();
        let size = TerminalSize { cols: 80, rows: 24 };
        state.sessions.create_session(a.clone(), size).unwrap();
        state
            .sessions
            .advance_next_pane_id_for_tests(rmux_core::PaneId::new(99));
        let mut session = state.sessions.session_setup(&a).unwrap();
        session
            .split_before(0, 0, SplitDirection::Vertical, true, false)
            .unwrap();
        session.select_pane(1).unwrap();
        if zoom {
            session.resize_pane(1, ResizePaneAdjustment::Zoom).unwrap();
        }
        state.sessions.create_session(b.clone(), size).unwrap();
        let window = state.sessions.session(&a).unwrap().window().id();
        state
            .sessions
            .session_setup(&b)
            .unwrap()
            .link_window_id(7, window, false, false)
            .unwrap();
        state.attach_window_link_slot(&a, 0, &b, 7);
        state
            .sessions
            .create_grouped_session_with_base_index(c.clone(), size, 0, a.clone())
            .unwrap();
        for (name, index, flags) in [
            (&a, 0, rmux_core::WINDOW_BELL),
            (&b, 7, rmux_core::WINDOW_ACTIVITY),
            (&c, 0, rmux_core::WINDOW_SILENCE),
        ] {
            state
                .sessions
                .session_setup(name)
                .unwrap()
                .add_winlink_alert_flags(index, flags);
            state
                .options
                .set_by_name(
                    OptionScopeSelector::Session(name.clone()),
                    "@local",
                    Some(name.to_string()),
                    SetOptionMode::Replace,
                    false,
                    false,
                    false,
                )
                .unwrap();
            state
                .options
                .set_by_name(
                    OptionScopeSelector::Window(WindowTarget::with_window(name.clone(), index)),
                    "@shared",
                    Some("window".into()),
                    SetOptionMode::Replace,
                    false,
                    false,
                    false,
                )
                .unwrap();
        }
        state.record_applied_window_resize(WindowTarget::with_window(b.clone(), 0));
        state.claim_applied_resize_layout_change(&WindowTarget::with_window(b, 0));
        state
    }

    pub(crate) fn window_selection_snapshot(&self) -> WindowSelectionSnapshot {
        for (name, _) in self
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            assert!(!self.terminals.contains_session(name));
        }
        assert!(self.transcripts.is_empty());
        let mut sessions = self
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .map(|(_, s)| s)
            .collect::<Vec<_>>();
        sessions.sort_by(|a, b| a.name().as_str().cmp(b.name().as_str()));
        WindowSelectionSnapshot {
            sessions: sessions.into_iter().map(|s| s.test_snapshot()).collect(),
            ancillary: vec![
                format!("{:?}", self.options),
                format!("{:?}", self.environment),
                format!("{:?}", self.hooks),
                format!("{:?}", self.pane_lifecycle),
                format!("{:?}", self.attached_terminal_pixels),
                format!("{:?}", self.dead_panes),
                format!("{:?}", self.auto_named_windows),
                format!("{:?}", self.window_link_groups),
                format!("{:?}", self.window_link_slots),
                format!("{:?}", self.window_link_occurrences),
                format!("{}", self.next_window_link_group_id),
                format!("{}", self.next_window_link_occurrence_id),
                format!("{:?}", self.applied_window_resizes),
            ],
        }
    }
}
