use super::pane_terminal_store::native_resize::{
    WindowMutationResizeError, WindowResizeCompensation,
};
use super::{session_not_found, HandlerState};
use rmux_core::ManagedWindowCheckpoint;
use rmux_core::{ManagedSessionStore, WindowSelection, WindowSwitch};
use rmux_proto::{PaneId, PaneTarget, RmuxError, SessionId, SessionName, WindowTarget};

#[derive(Debug, Clone)]
pub(crate) enum WindowRuntimeResizeScope {
    Window,
    Pane(PaneId),
    Panes(Vec<PaneId>),
}

impl HandlerState {
    pub(crate) fn mutate_switch_window_and_resize_terminal_with_family<T, F>(
        &mut self,
        target: &WindowTarget,
        expected_session_id: Option<SessionId>,
        touch_attached: bool,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(WindowSwitch<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            target.session_name(),
            target.window_index(),
            |store| {
                if expected_session_id.is_some_and(|expected| {
                    store
                        .session(target.session_name())
                        .is_none_or(|session| session.id() != expected)
                }) {
                    return Err(session_not_found(target.session_name()));
                }
                let switch = if touch_attached {
                    store.touched_window_switch_mut(target)?
                } else {
                    store.window_switch_mut(target)?
                };
                mutate(switch)
            },
            |_| true,
        )
        .map_err(RmuxError::from)
    }

    pub(crate) fn mutate_window_and_resize_terminal_with_family<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(WindowSelection<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_window_and_resize_terminal_with_family_if(target, mutate, |_| true)
            .map_err(RmuxError::from)
    }

    pub(crate) fn mutate_window_and_resize_terminal_with_family_if<T, F, P>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
        should_resize: P,
    ) -> Result<(T, Vec<SessionName>), WindowMutationResizeError>
    where
        F: FnOnce(WindowSelection<'_>) -> Result<T, RmuxError>,
        P: FnOnce(&T) -> bool,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            target.session_name(),
            target.window_index(),
            |store| mutate(store.window_selection_mut(target)?),
            should_resize,
        )
    }

    /// [`Self::mutate_window_and_resize_terminal_with_family_if`] for a request
    /// that names `named`, a pane of the window it changes. The resize reads
    /// the window's other panes under the sweep policy; when the change
    /// resizes, a poisoned named pane refuses it and the window is restored,
    /// so the pane's own operations keep failing fast.
    pub(crate) fn mutate_window_naming_pane_and_resize_terminal_with_family_if<T, F, P>(
        &mut self,
        named: &PaneTarget,
        mutate: F,
        should_resize: P,
    ) -> Result<(T, Vec<SessionName>), WindowMutationResizeError>
    where
        F: FnOnce(WindowSelection<'_>) -> Result<T, RmuxError>,
        P: FnOnce(&T) -> bool,
    {
        let target = WindowTarget::with_window(named.session_name().clone(), named.window_index());
        let named_pane_id = crate::pane_terminal_lookup::pane_id_for_target(
            &self.sessions,
            named.session_name(),
            named.window_index(),
            named.pane_index(),
        )
        .ok();
        self.mutate_store_and_resize_window_terminal_with_family_scoped(
            named.session_name(),
            named.window_index(),
            |store| mutate(store.window_selection_mut(&target)?),
            |state, result| {
                if !should_resize(result) {
                    return Ok(None);
                }
                if let Some(pane_id) = named_pane_id {
                    state.ensure_pane_id_available(
                        named.session_name(),
                        named.window_index(),
                        pane_id,
                    )?;
                }
                Ok(Some(WindowRuntimeResizeScope::Window))
            },
        )
    }

    pub(crate) fn mutate_window_and_synchronize_family<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(WindowSelection<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_synchronize_window_family_with_snapshot(
            target.session_name(),
            target.window_index(),
            |store| mutate(store.window_selection_mut(target)?),
        )
        .map(|(result, sessions, _)| (result, sessions))
    }

    /// The primary chokepoint for mutations scoped to one stored window.
    ///
    /// tmux 3.7b keeps the equivalent invariant inside `resize_window()`: a
    /// window whose size actually changed always publishes
    /// `window-layout-changed` and then `window-resized`, and control clients see
    /// the former as `%layout-change`. Publication here is asynchronous, so a
    /// window whose size really moved is recorded on the state and drained by
    /// `RequestHandler::publish_applied_window_resizes`. Join/move transfers use
    /// the mandatory all-window snapshot boundary in `session_mutation` for the same
    /// invariant across stable window identities.
    pub(in crate::pane_terminals) fn mutate_store_and_resize_window_terminal_with_family_if<
        T,
        F,
        P,
    >(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
        should_resize: P,
    ) -> Result<(T, Vec<SessionName>), WindowMutationResizeError>
    where
        F: FnOnce(&mut ManagedSessionStore) -> Result<T, RmuxError>,
        P: FnOnce(&T) -> bool,
    {
        self.mutate_store_and_resize_window_terminal_with_family_scoped(
            session_name,
            window_index,
            mutate,
            |_, result| Ok(should_resize(result).then_some(WindowRuntimeResizeScope::Window)),
        )
    }

    pub(crate) fn mutate_store_and_resize_window_terminal_with_family_scoped<T, F, P>(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
        resize_scope: P,
    ) -> Result<(T, Vec<SessionName>), WindowMutationResizeError>
    where
        F: FnOnce(&mut ManagedSessionStore) -> Result<T, RmuxError>,
        P: FnOnce(&Self, &T) -> Result<Option<WindowRuntimeResizeScope>, RmuxError>,
    {
        let geometry_before = self.window_content_size(session_name, window_index);
        let (result, synchronized_sessions, snapshot) = self
            .mutate_store_and_synchronize_window_family_with_snapshot(
                session_name,
                window_index,
                mutate,
            )?;

        let resize_scope = match resize_scope(self, &result) {
            Ok(scope) => scope,
            Err(error) => {
                self.sessions.restore_window_checkpoint(
                    &WindowTarget::with_window(session_name.clone(), window_index),
                    snapshot,
                )?;
                return Err(error.into());
            }
        };
        let Some(resize_scope) = resize_scope else {
            self.record_window_geometry_change(session_name, window_index, geometry_before);
            return Ok((result, synchronized_sessions));
        };

        if let Err(original) = self.resize_window_terminal_runtime_scoped(
            session_name,
            window_index,
            resize_scope.clone(),
        ) {
            self.sessions.restore_window_checkpoint(
                &WindowTarget::with_window(session_name.clone(), window_index),
                snapshot,
            )?;
            let compensation = self.resize_window_terminal_runtime_scoped(
                session_name,
                window_index,
                resize_scope,
            );

            let failure =
                WindowMutationResizeError::Compensation(Box::new(WindowResizeCompensation {
                    target: WindowTarget::with_window(session_name.clone(), window_index),
                    original,
                    compensation,
                }));
            #[cfg(all(test, target_os = "macos"))]
            self.check_resize_compensation_for_test(&failure);
            return Err(failure);
        }

        for synchronized_session in &synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(synchronized_session);
        }

        self.record_window_geometry_change(session_name, window_index, geometry_before);
        Ok((result, synchronized_sessions))
    }

    fn record_window_geometry_change(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        geometry_before: Option<rmux_proto::TerminalSize>,
    ) {
        let geometry_after = self.window_content_size(session_name, window_index);
        if geometry_after.is_none() || geometry_after == geometry_before {
            return;
        }
        self.record_applied_window_resize(WindowTarget::with_window(
            session_name.clone(),
            window_index,
        ));
    }

    fn window_content_size(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Option<rmux_proto::TerminalSize> {
        self.sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .map(rmux_core::Window::size)
    }

    pub(in crate::pane_terminals) fn mutate_store_and_synchronize_window_family_with_snapshot<
        T,
        F,
    >(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>, ManagedWindowCheckpoint), RmuxError>
    where
        F: FnOnce(&mut ManagedSessionStore) -> Result<T, RmuxError>,
    {
        let target = WindowTarget::with_window(session_name.clone(), window_index);
        let snapshot = self.sessions.checkpoint_window(&target)?;
        let result = match mutate(&mut self.sessions) {
            Ok(result) => result,
            Err(error) => {
                self.sessions.restore_window_checkpoint(&target, snapshot)?;
                return Err(error);
            }
        };
        let synchronized_sessions =
            self.window_linked_session_family_list(session_name, window_index);
        Ok((result, synchronized_sessions, snapshot))
    }
}

#[cfg(test)]
#[path = "window_selection_tests.rs"]
mod tests;
