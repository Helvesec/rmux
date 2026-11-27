#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::fmt::Display;

use rmux_core::{Window, WindowSwitch};
use rmux_proto::{PaneId, PaneTarget, RmuxError, SessionId, SessionName, WindowId, WindowTarget};

use crate::pane_terminals::{session_not_found, HandlerState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::handler) enum SwitchTargetSelection {
    Window {
        target: WindowTarget,
        window_id: WindowId,
    },
    Pane {
        target: PaneTarget,
        window_id: WindowId,
        pane_id: PaneId,
        pane_output_generation: Option<u64>,
        zoom: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ValidatedSwitchTarget {
    Window {
        window_index: u32,
    },
    Pane {
        window_index: u32,
        pane_index: u32,
        zoom_pane_to_restore: Option<u32>,
    },
}

impl SwitchTargetSelection {
    pub(in crate::handler) fn session_name(&self) -> &SessionName {
        match self {
            Self::Window { target, .. } => target.session_name(),
            Self::Pane { target, .. } => target.session_name(),
        }
    }

    pub(in crate::handler) fn window_target(&self) -> WindowTarget {
        let window_index = match self {
            Self::Window { target, .. } => target.window_index(),
            Self::Pane { target, .. } => target.window_index(),
        };
        WindowTarget::with_window(self.session_name().clone(), window_index)
    }

    pub(in crate::handler) fn validate_for_session_identity(
        &self,
        state: &HandlerState,
        expected_session_name: &SessionName,
        expected_session_id: SessionId,
    ) -> Result<(), RmuxError> {
        if self.session_name() != expected_session_name {
            return Err(RmuxError::Server(
                "switch target selection changed sessions before commit".to_owned(),
            ));
        }
        let session = state
            .sessions
            .session(expected_session_name)
            .filter(|session| session.id() == expected_session_id)
            .ok_or_else(|| session_not_found(expected_session_name))?;
        self.validate_for_session(session)?;
        if let Self::Pane {
            target,
            pane_id,
            pane_output_generation: Some(expected_generation),
            ..
        } = self
        {
            let current_generation = state.pane_output_generation_for_target(target, *pane_id);
            if current_generation != *expected_generation {
                return Err(RmuxError::invalid_target(
                    target.to_string(),
                    "pane output generation changed before switch commit",
                ));
            }
        }
        Ok(())
    }

    pub(in crate::handler) fn apply_to_state(
        &self,
        state: &mut HandlerState,
    ) -> Result<Vec<SessionName>, RmuxError> {
        let target = self.window_target();
        let (_, refresh_sessions) = state.mutate_switch_window_and_resize_terminal_with_family(
            &target,
            None,
            false,
            |mut switch| self.apply_to_window_switch(&mut switch),
        )?;
        Ok(refresh_sessions)
    }

    pub(in crate::handler) fn apply_to_window_switch(
        &self,
        switch: &mut WindowSwitch<'_>,
    ) -> Result<(), RmuxError> {
        let (target, expected_window_id) = match self {
            Self::Window { target, window_id } => (target.to_string(), *window_id),
            Self::Pane {
                target, window_id, ..
            } => (target.to_string(), *window_id),
        };
        if switch.window_id() != expected_window_id {
            return Err(RmuxError::invalid_target(
                target,
                "window identity changed before switch commit",
            ));
        }

        match self {
            Self::Window { .. } => {
                switch.select_window();
                Ok(())
            }
            Self::Pane {
                target,
                pane_id,
                zoom,
                ..
            } => {
                let current_pane_id = switch.pane_id(target.pane_index()).ok_or_else(|| {
                    RmuxError::invalid_target(
                        target.to_string(),
                        "pane index does not exist in session",
                    )
                })?;
                if current_pane_id != *pane_id {
                    return Err(RmuxError::invalid_target(
                        target.to_string(),
                        "pane identity changed before switch commit",
                    ));
                }
                switch.select_window_and_pane(target.pane_index(), *zoom)
            }
        }
    }

    pub(in crate::handler) fn apply_to_presentation(
        &self,
        session: &mut rmux_core::ManagedSessionPresentation<'_>,
    ) -> Result<(), RmuxError> {
        self.validate_for_session(session.view())?;
        self.apply_to_window_switch(
            &mut session.window_switch_mut(self.window_target().window_index())?,
        )
    }

    fn validate_for_session(
        &self,
        session: rmux_core::ManagedSessionView<'_>,
    ) -> Result<ValidatedSwitchTarget, RmuxError> {
        match self {
            Self::Window { target, window_id } => {
                validated_window(session, target, target.window_index(), *window_id)?;
                Ok(ValidatedSwitchTarget::Window {
                    window_index: target.window_index(),
                })
            }
            Self::Pane {
                target,
                window_id,
                pane_id,
                zoom,
                ..
            } => {
                let window = validated_window(session, target, target.window_index(), *window_id)?;
                let pane = window.pane(target.pane_index()).ok_or_else(|| {
                    RmuxError::invalid_target(
                        target.to_string(),
                        "pane index does not exist in session",
                    )
                })?;
                if pane.id() != *pane_id {
                    return Err(RmuxError::invalid_target(
                        target.to_string(),
                        "pane identity changed before switch commit",
                    ));
                }
                Ok(ValidatedSwitchTarget::Pane {
                    window_index: target.window_index(),
                    pane_index: target.pane_index(),
                    zoom_pane_to_restore: (window.is_zoomed() && *zoom)
                        .then(|| window.active_pane_index()),
                })
            }
        }
    }
}

fn validated_window<'a>(
    session: rmux_core::ManagedSessionView<'a>,
    target: &impl Display,
    window_index: u32,
    expected_window_id: WindowId,
) -> Result<&'a Window, RmuxError> {
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(target.to_string(), "window index does not exist in session")
    })?;
    if window.id() != expected_window_id {
        return Err(RmuxError::invalid_target(
            target.to_string(),
            "window identity changed before switch commit",
        ));
    }
    Ok(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    use rmux_core::WINLINK_BELL;
    use rmux_proto::TerminalSize;

    #[test]
    fn validation_is_read_only_before_application_mutates_selection_state() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("read-only-selection").expect("valid session name");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session is created");

        let (session_id, window_index, window_id, pane_index, pane_id, zoomed_pane_index) = {
            let mut session = state
                .sessions
                .session_setup(&session_name)
                .expect("session exists");
            let (window_index, _) = session
                .create_window(TerminalSize { cols: 80, rows: 24 })
                .expect("second window is created");
            let zoomed_pane_index = session
                .split_pane_in_window(window_index, 0)
                .expect("target window is split");
            session
                .toggle_zoom_in_window(window_index, zoomed_pane_index)
                .expect("target window is zoomed");
            session
                .select_window(window_index)
                .expect("target window is selected once");
            session.select_window(0).expect("source window is restored");
            assert!(session.add_winlink_alert_flags(window_index, WINLINK_BELL));
            let window = session
                .window_at(window_index)
                .expect("target window exists");
            let target_pane_index = 0;
            let pane_id = window
                .pane(target_pane_index)
                .expect("target pane exists")
                .id();
            (
                session.id(),
                window_index,
                window.id(),
                target_pane_index,
                pane_id,
                zoomed_pane_index,
            )
        };
        let target = PaneTarget::with_window(session_name.clone(), window_index, pane_index);
        let output_generation = state.pane_output_generation_for_target(&target, pane_id);
        let pane_selection = SwitchTargetSelection::Pane {
            target,
            window_id,
            pane_id,
            pane_output_generation: Some(output_generation),
            zoom: true,
        };
        let window_selection = SwitchTargetSelection::Window {
            target: WindowTarget::with_window(session_name.clone(), window_index),
            window_id,
        };
        let selection_state = |session: rmux_core::ManagedSessionView<'_>| {
            let window = session
                .window_at(window_index)
                .expect("target window exists");
            (
                session.active_window_index(),
                session.last_window_index(),
                session.winlink_alert_flags(window_index),
                window.is_zoomed(),
                window.active_pane_index(),
                window.last_pane_index(),
                window.visible_layout_dump(),
            )
        };
        let before = selection_state(
            state
                .sessions
                .session(&session_name)
                .expect("session exists"),
        );

        window_selection
            .validate_for_session_identity(&state, &session_name, session_id)
            .expect("window selection is valid");
        pane_selection
            .validate_for_session_identity(&state, &session_name, session_id)
            .expect("pane selection is valid");

        assert_eq!(
            selection_state(
                state
                    .sessions
                    .session(&session_name)
                    .expect("session exists")
            ),
            before,
            "read-only validation must not mutate selection, history, alerts, or zoom"
        );

        pane_selection
            .apply_to_window_switch(
                &mut state
                    .sessions
                    .window_switch_mut(&pane_selection.window_target())
                    .expect("target exists"),
            )
            .expect("validated pane selection applies");
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        assert_eq!(session.active_window_index(), window_index);
        assert_eq!(session.last_window_index(), Some(0));
        assert!(session.winlink_alert_flags(window_index).is_empty());
        let window = session
            .window_at(window_index)
            .expect("target window exists");
        assert!(window.is_zoomed(), "requested zoom must be restored");
        assert_eq!(window.active_pane_index(), pane_index);
        assert_eq!(window.last_pane_index(), Some(zoomed_pane_index));
    }

    #[test]
    fn validation_preserves_identity_and_generation_error_precedence() {
        let mut state = HandlerState::default();
        let session_name = SessionName::new("selection-errors").expect("valid session name");
        state
            .sessions
            .create_session(session_name.clone(), TerminalSize { cols: 80, rows: 24 })
            .expect("session is created");
        let session = state
            .sessions
            .session(&session_name)
            .expect("session exists");
        let session_id = session.id();
        let window = session.window_at(0).expect("window exists");
        let window_id = window.id();
        let pane_id = window.pane(0).expect("pane exists").id();
        let target = PaneTarget::with_window(session_name.clone(), 0, 0);
        let stale_generation = state
            .pane_output_generation_for_target(&target, pane_id)
            .wrapping_add(1);

        let validate = |window_id, pane_id| {
            SwitchTargetSelection::Pane {
                target: target.clone(),
                window_id,
                pane_id,
                pane_output_generation: Some(stale_generation),
                zoom: false,
            }
            .validate_for_session_identity(&state, &session_name, session_id)
            .expect_err("selection must be stale")
        };

        let missing_window_target = PaneTarget::with_window(session_name.clone(), 1, 0);
        assert_eq!(
            SwitchTargetSelection::Pane {
                target: missing_window_target.clone(),
                window_id,
                pane_id,
                pane_output_generation: Some(stale_generation),
                zoom: false,
            }
            .validate_for_session_identity(&state, &session_name, session_id)
            .expect_err("missing window must be rejected"),
            RmuxError::invalid_target(
                missing_window_target.to_string(),
                "window index does not exist in session"
            )
        );
        assert_eq!(
            validate(WindowId::new(window_id.as_u32().wrapping_add(1)), pane_id),
            RmuxError::invalid_target(
                target.to_string(),
                "window identity changed before switch commit"
            )
        );
        let missing_pane_target = PaneTarget::with_window(session_name.clone(), 0, 1);
        assert_eq!(
            SwitchTargetSelection::Pane {
                target: missing_pane_target.clone(),
                window_id,
                pane_id,
                pane_output_generation: Some(stale_generation),
                zoom: false,
            }
            .validate_for_session_identity(&state, &session_name, session_id)
            .expect_err("missing pane must be rejected"),
            RmuxError::invalid_target(
                missing_pane_target.to_string(),
                "pane index does not exist in session"
            )
        );
        assert_eq!(
            validate(window_id, PaneId::new(pane_id.as_u32().wrapping_add(1))),
            RmuxError::invalid_target(
                target.to_string(),
                "pane identity changed before switch commit"
            )
        );
        assert_eq!(
            validate(window_id, pane_id),
            RmuxError::invalid_target(
                target.to_string(),
                "pane output generation changed before switch commit"
            )
        );
    }
}
