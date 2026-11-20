use rmux_proto::{PaneTarget, RmuxError, SessionName, Target, WindowTarget};

use crate::{Pane, Session, SessionStore, Window};

impl SessionStore {
    /// Resolves any exact target to its owning session.
    pub fn resolve_session(&self, target: &Target) -> Result<&Session, RmuxError> {
        self.session(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))
    }

    /// Resolves any exact target to its owning mutable session.
    pub fn resolve_session_mut(&mut self, target: &Target) -> Result<&mut Session, RmuxError> {
        self.session_mut(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))
    }

    /// Resolves any exact target to the addressed session window.
    pub fn resolve_window(&self, target: &Target) -> Result<&Window, RmuxError> {
        let session = self.resolve_session(target)?;
        session
            .window_at(target_window_index(session, target))
            .ok_or_else(|| window_not_found(target))
    }

    /// Resolves any exact target to the addressed mutable session window.
    pub fn resolve_window_mut(&mut self, target: &Target) -> Result<&mut Window, RmuxError> {
        let session = self.resolve_session_mut(target)?;
        let window_index = target_window_index(session, target);
        session
            .window_at_mut(window_index)
            .ok_or_else(|| window_not_found(target))
    }

    /// Resolves an exact pane target to an immutable pane reference.
    pub fn resolve_pane(&self, target: &Target) -> Result<&Pane, RmuxError> {
        match target {
            Target::Pane(pane_target) => self
                .resolve_window(target)?
                .pane(pane_target.pane_index())
                .ok_or_else(|| pane_not_found(pane_target)),
            _ => Err(pane_target_required(target)),
        }
    }

    /// Resolves an exact pane target to a mutable pane reference.
    pub fn resolve_pane_mut(&mut self, target: &Target) -> Result<&mut Pane, RmuxError> {
        match target {
            Target::Pane(pane_target) => self
                .resolve_window_mut(target)?
                .pane_mut(pane_target.pane_index())
                .ok_or_else(|| pane_not_found(pane_target)),
            _ => Err(pane_target_required(target)),
        }
    }

    /// Resolves a target to the effective pane used by pane-oriented commands.
    ///
    /// Session and window targets resolve to the session's active pane. Pane
    /// targets resolve to the explicitly addressed pane.
    pub fn resolve_effective_pane(&self, target: &Target) -> Result<&Pane, RmuxError> {
        match target {
            Target::Session(_) => self
                .resolve_session(target)?
                .active_pane()
                .ok_or_else(|| active_pane_not_found(target)),
            Target::Window(_) => self
                .resolve_window(target)?
                .active_pane()
                .ok_or_else(|| active_pane_not_found(target)),
            Target::Pane(_) => self.resolve_pane(target),
        }
    }

    /// Resolves a target to the mutable effective pane used by pane-oriented
    /// commands.
    ///
    /// Session and window targets resolve to the session's active pane. Pane
    /// targets resolve to the explicitly addressed pane.
    pub fn resolve_effective_pane_mut(&mut self, target: &Target) -> Result<&mut Pane, RmuxError> {
        match target {
            Target::Session(_) => {
                let session = self.resolve_session_mut(target)?;
                let active_pane_index = session.active_pane_index();
                session
                    .window_mut()
                    .pane_mut(active_pane_index)
                    .ok_or_else(|| active_pane_not_found(target))
            }
            Target::Window(_) => {
                let window = self.resolve_window_mut(target)?;
                let active_pane_index = window.active_pane_index();
                window
                    .pane_mut(active_pane_index)
                    .ok_or_else(|| active_pane_not_found(target))
            }
            Target::Pane(_) => self.resolve_pane_mut(target),
        }
    }
}

impl crate::ManagedSessionStore {
    /// Resolves an exact target by borrowing canonical managed state.
    pub fn resolve_session(
        &self,
        target: &Target,
    ) -> Result<crate::ManagedSessionView<'_>, RmuxError> {
        self.session(target.session_name())
            .ok_or_else(|| session_not_found(target.session_name()))
    }

    /// Resolves an exact target by borrowing canonical managed state.
    pub fn resolve_window(&self, target: &Target) -> Result<&Window, RmuxError> {
        let session = self.resolve_session(target)?;
        session
            .window_at(match target {
                Target::Session(_) => session.active_window_index(),
                Target::Window(t) => t.window_index(),
                Target::Pane(t) => t.window_index(),
            })
            .ok_or_else(|| window_not_found(target))
    }

    /// Resolves an exact target by borrowing canonical managed state.
    pub fn resolve_pane(&self, target: &Target) -> Result<&Pane, RmuxError> {
        match target {
            Target::Pane(pane_target) => self
                .resolve_window(target)?
                .pane(pane_target.pane_index())
                .ok_or_else(|| pane_not_found(pane_target)),
            _ => Err(pane_target_required(target)),
        }
    }

    /// Resolves an exact target by borrowing canonical managed state.
    pub fn resolve_effective_pane(&self, target: &Target) -> Result<&Pane, RmuxError> {
        match target {
            Target::Session(_) => self
                .resolve_session(target)?
                .active_pane()
                .ok_or_else(|| active_pane_not_found(target)),
            Target::Window(_) => self
                .resolve_window(target)?
                .active_pane()
                .ok_or_else(|| active_pane_not_found(target)),
            Target::Pane(_) => self.resolve_pane(target),
        }
    }
}

fn session_not_found(session_name: &SessionName) -> RmuxError {
    RmuxError::SessionNotFound(session_name.to_string())
}

fn pane_not_found(target: &PaneTarget) -> RmuxError {
    RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
}

fn window_not_found(target: &Target) -> RmuxError {
    RmuxError::invalid_target(
        window_target_value(target),
        "window index does not exist in session",
    )
}

fn pane_target_required(target: &Target) -> RmuxError {
    RmuxError::invalid_target(target.to_string(), "pane target required")
}

fn active_pane_not_found(target: &Target) -> RmuxError {
    RmuxError::invalid_target(target.to_string(), "active pane does not exist in session")
}

fn window_target_value(target: &Target) -> String {
    match target {
        Target::Session(session_name) => session_name.to_string(),
        Target::Window(target) => target.to_string(),
        Target::Pane(target) => {
            WindowTarget::with_window(target.session_name().clone(), target.window_index())
                .to_string()
        }
    }
}

fn target_window_index(session: &Session, target: &Target) -> u32 {
    match target {
        Target::Session(_) => session.active_window_index(),
        Target::Window(target) => target.window_index(),
        Target::Pane(target) => target.window_index(),
    }
}

#[cfg(test)]
mod tests {
    use crate::{PaneGeometry, SessionStore};
    use rmux_proto::{SessionName, Target, TerminalSize};

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn populated_store() -> SessionStore {
        let mut store = SessionStore::new();
        store
            .create_session(
                session_name("alpha"),
                TerminalSize {
                    cols: 120,
                    rows: 40,
                },
            )
            .expect("session insert succeeds");

        let session_target = Target::parse("alpha").expect("valid session target");
        store
            .resolve_session_mut(&session_target)
            .expect("session exists")
            .split_active_pane()
            .expect("split succeeds");
        store
    }

    fn store_with_selected_second_pane() -> SessionStore {
        let mut store = populated_store();
        let target = Target::parse("alpha").expect("valid session target");
        store
            .resolve_session_mut(&target)
            .expect("session exists")
            .select_pane(1)
            .expect("pane 1 exists");
        store
    }

    fn multi_window_store() -> SessionStore {
        let mut store = SessionStore::new();
        store
            .create_session(
                session_name("alpha"),
                TerminalSize {
                    cols: 120,
                    rows: 40,
                },
            )
            .expect("session insert succeeds");

        let session_target = Target::parse("alpha").expect("valid session target");
        let session = store
            .resolve_session_mut(&session_target)
            .expect("session exists");
        session
            .insert_window_with_initial_pane(5, TerminalSize { cols: 90, rows: 30 })
            .expect("window 5 insert succeeds");
        session
            .split_pane_in_window(5, 0)
            .expect("window 5 split succeeds");
        session
            .select_pane_in_window(5, 1)
            .expect("window 5 pane selection succeeds");
        session
            .select_window(5)
            .expect("window 5 selection succeeds");
        store
    }

    #[test]
    fn resolve_session_accepts_session_targets() {
        let store = populated_store();
        let target = Target::parse("alpha").expect("valid session target");

        let session = store.resolve_session(&target).expect("session exists");

        assert_eq!(session.name().as_str(), "alpha");
    }

    #[test]
    fn resolve_session_accepts_pane_targets() {
        let store = populated_store();
        let target = Target::parse("alpha:0.1").expect("valid pane target");

        let session = store.resolve_session(&target).expect("session exists");

        assert_eq!(session.name().as_str(), "alpha");
    }

    #[test]
    fn resolve_session_reports_missing_sessions() {
        let store = SessionStore::new();
        let target = Target::parse("alpha").expect("valid session target");

        let error = store
            .resolve_session(&target)
            .expect_err("session should not exist");

        assert_eq!(error.to_string(), "session not found: alpha");
    }

    #[test]
    fn resolve_window_accepts_window_targets() {
        let store = populated_store();
        let target = Target::parse("alpha:0").expect("valid window target");

        let window = store.resolve_window(&target).expect("window exists");

        assert_eq!(window.pane_count(), 2);
    }

    #[test]
    fn resolve_window_accepts_session_targets() {
        let store = populated_store();
        let target = Target::parse("alpha").expect("valid session target");

        let window = store.resolve_window(&target).expect("window exists");

        assert_eq!(window.pane_count(), 2);
    }

    #[test]
    fn resolve_window_accepts_pane_targets() {
        let store = populated_store();
        let target = Target::parse("alpha:0.1").expect("valid pane target");

        let window = store.resolve_window(&target).expect("window exists");

        assert_eq!(window.pane_count(), 2);
    }

    #[test]
    fn resolve_window_accepts_non_zero_window_targets() {
        let store = multi_window_store();
        let target = Target::parse("alpha:5").expect("valid window target");

        let window = store.resolve_window(&target).expect("window exists");

        assert_eq!(window.pane_count(), 2);
    }

    #[test]
    fn resolve_window_reports_missing_window_after_successful_parse() {
        let store = populated_store();
        let target = Target::parse("alpha:5").expect("valid window target");

        let error = store
            .resolve_window(&target)
            .expect_err("window should not exist");

        assert_eq!(
            error.to_string(),
            "invalid target 'alpha:5': window index does not exist in session"
        );
    }

    #[test]
    fn resolve_window_reports_missing_window_for_pane_targets_with_window_context() {
        let store = populated_store();
        let target = Target::parse("alpha:5.0").expect("valid pane target");

        let error = store
            .resolve_window(&target)
            .expect_err("window should not exist");

        assert_eq!(
            error.to_string(),
            "invalid target 'alpha:5': window index does not exist in session"
        );
    }

    #[test]
    fn resolve_window_mut_returns_a_mutable_window_reference() {
        let mut store = populated_store();
        let target = Target::parse("alpha:0").expect("valid window target");

        let window = store
            .resolve_window_mut(&target)
            .expect("mutable window exists");
        let pane_one_position = window.pane_position(1).expect("pane 1 exists");
        let new_index = window.split_after_position(pane_one_position);

        assert_eq!(new_index, 2);
        assert_eq!(window.pane_count(), 3);
    }

    #[test]
    fn resolve_pane_returns_the_addressed_pane() {
        let store = populated_store();
        let target = Target::parse("alpha:0.1").expect("valid pane target");

        let pane = store.resolve_pane(&target).expect("pane exists");

        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_pane_mut_returns_a_mutable_reference() {
        let mut store = populated_store();
        let target = Target::parse("alpha:0.1").expect("valid pane target");

        let _ = store
            .resolve_pane_mut(&target)
            .expect("pane exists")
            .target(&session_name("alpha"));

        let pane = store.resolve_pane(&target).expect("pane still exists");
        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_pane_rejects_non_pane_targets() {
        let store = populated_store();
        let target = Target::parse("alpha").expect("valid session target");

        let error = store
            .resolve_pane(&target)
            .expect_err("pane resolution requires a pane target");

        assert_eq!(
            error.to_string(),
            "invalid target 'alpha': pane target required"
        );
    }

    #[test]
    fn resolve_pane_reports_missing_pane_indices() {
        let store = populated_store();
        let target = Target::parse("alpha:0.9").expect("valid pane target");

        let error = store
            .resolve_pane(&target)
            .expect_err("pane index should not exist");

        assert_eq!(
            error.to_string(),
            "invalid target 'alpha:0.9': pane index does not exist in session"
        );
    }

    #[test]
    fn resolve_pane_reports_missing_sessions_before_missing_panes() {
        let store = SessionStore::new();
        let target = Target::parse("alpha:0.0").expect("valid pane target");

        let error = store
            .resolve_pane(&target)
            .expect_err("session should not exist");

        assert_eq!(error.to_string(), "session not found: alpha");
    }

    #[test]
    fn resolve_effective_pane_maps_session_targets_to_the_active_pane() {
        let store = store_with_selected_second_pane();
        let target = Target::parse("alpha").expect("valid session target");

        let pane = store
            .resolve_effective_pane(&target)
            .expect("active pane exists");

        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_effective_pane_maps_session_targets_to_the_active_window() {
        let store = multi_window_store();
        let target = Target::parse("alpha").expect("valid session target");

        let pane = store
            .resolve_effective_pane(&target)
            .expect("active pane exists");

        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_effective_pane_maps_window_targets_to_the_active_pane() {
        let store = store_with_selected_second_pane();
        let target = Target::parse("alpha:0").expect("valid window target");

        let pane = store
            .resolve_effective_pane(&target)
            .expect("active pane exists");

        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_effective_pane_uses_the_addressed_window_active_pane() {
        let store = multi_window_store();
        let target = Target::parse("alpha:5").expect("valid window target");

        let pane = store
            .resolve_effective_pane(&target)
            .expect("active pane exists");

        assert_eq!(pane.index(), 1);
    }

    #[test]
    fn resolve_effective_pane_keeps_explicit_pane_targets_exact() {
        let store = store_with_selected_second_pane();
        let target = Target::parse("alpha:0.0").expect("valid pane target");

        let pane = store
            .resolve_effective_pane(&target)
            .expect("addressed pane exists");

        assert_eq!(pane.index(), 0);
    }

    #[test]
    fn resolve_effective_pane_mut_updates_the_active_pane_for_window_targets() {
        let mut store = store_with_selected_second_pane();
        let target = Target::parse("alpha:0").expect("valid window target");
        let replacement = PaneGeometry::new(9, 8, 7, 6);

        store
            .resolve_effective_pane_mut(&target)
            .expect("active pane exists")
            .set_geometry(replacement);

        let pane = store
            .resolve_effective_pane(&target)
            .expect("active pane still exists");
        assert_eq!(pane.geometry(), replacement);
    }
}
