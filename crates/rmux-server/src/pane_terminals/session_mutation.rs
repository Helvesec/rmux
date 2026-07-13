use std::collections::{HashMap, HashSet};

use rmux_core::{EnvironmentStore, HookStore, OptionStore, PaneId, Session, SessionStore};
use rmux_proto::{RmuxError, SessionName, TerminalPixels};

use super::{
    session_not_found, HandlerState, PaneExitMetadata, PaneLifecycleState, WindowLinkGroup,
    WindowLinkSlot,
};

pub(crate) struct SessionTransferSnapshot {
    sessions: SessionStore,
    options: OptionStore,
    environment: EnvironmentStore,
    hooks: HookStore,
    pane_lifecycle: HashMap<PaneId, PaneLifecycleState>,
    attached_terminal_pixels: HashMap<SessionName, TerminalPixels>,
    dead_panes: HashMap<SessionName, HashMap<PaneId, PaneExitMetadata>>,
    auto_named_windows: HashSet<(SessionName, u32)>,
    window_link_groups: HashMap<u64, WindowLinkGroup>,
    window_link_slots: HashMap<WindowLinkSlot, u64>,
    window_link_occurrences: HashMap<WindowLinkSlot, super::WindowLinkOccurrenceId>,
    next_window_link_group_id: u64,
    next_window_link_occurrence_id: u64,
}

impl SessionTransferSnapshot {
    pub(crate) fn capture(state: &HandlerState) -> Self {
        Self {
            sessions: state.sessions.clone(),
            options: state.options.clone(),
            environment: state.environment.clone(),
            hooks: state.hooks.clone(),
            pane_lifecycle: state.pane_lifecycle.clone(),
            attached_terminal_pixels: state.attached_terminal_pixels.clone(),
            dead_panes: state.dead_panes.clone(),
            auto_named_windows: state.auto_named_windows.clone(),
            window_link_groups: state.window_link_groups.clone(),
            window_link_slots: state.window_link_slots.clone(),
            window_link_occurrences: state.window_link_occurrences.clone(),
            next_window_link_group_id: state.next_window_link_group_id,
            next_window_link_occurrence_id: state.next_window_link_occurrence_id,
        }
    }

    pub(crate) fn restore(self, state: &mut HandlerState) {
        state.sessions = self.sessions;
        state.options = self.options;
        state.environment = self.environment;
        state.hooks = self.hooks;
        state.pane_lifecycle = self.pane_lifecycle;
        state.attached_terminal_pixels = self.attached_terminal_pixels;
        state.dead_panes = self.dead_panes;
        state.auto_named_windows = self.auto_named_windows;
        state.window_link_groups = self.window_link_groups;
        state.window_link_slots = self.window_link_slots;
        state.window_link_occurrences = self.window_link_occurrences;
        state.next_window_link_group_id = self.next_window_link_group_id;
        state.next_window_link_occurrence_id = self.next_window_link_occurrence_id;
    }
}

impl HandlerState {
    pub(crate) fn mutate_session_and_resize_active_window_terminal<T, F>(
        &mut self,
        session_name: &SessionName,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut Session) -> Result<T, RmuxError>,
    {
        let active_window_index = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?
            .active_window_index();
        self.mutate_session_and_resize_window_terminal(session_name, active_window_index, mutate)
    }

    pub(crate) fn mutate_session_and_resize_window_terminal<T, F>(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut Session) -> Result<T, RmuxError>,
    {
        let snapshot = SessionTransferSnapshot::capture(self);
        let result = {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            mutate(session)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };
        let synchronized_sessions =
            match self.synchronize_linked_window_family_from_slot(session_name, window_index) {
                Ok(synchronized_sessions) => synchronized_sessions,
                Err(error) => {
                    snapshot.restore(self);
                    return Err(error);
                }
            };

        if let Err(error) = self.resize_window_terminal_runtime(session_name, window_index) {
            snapshot.restore(self);
            self.resize_window_terminal_runtime(session_name, window_index)
                .map_err(|rollback_error| {
                    RmuxError::Server(format!(
                        "failed to roll back window runtime for {session_name}:{window_index} after {error}: {rollback_error}"
                    ))
                })?;
            return Err(error);
        }

        for synchronized_session in synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(&synchronized_session);
        }
        Ok(result)
    }

    pub(crate) fn mutate_session_and_resize_terminals<T, F>(
        &mut self,
        session_name: &SessionName,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut Session) -> Result<T, RmuxError>,
    {
        let previous_session = self
            .sessions
            .session(session_name)
            .cloned()
            .ok_or_else(|| session_not_found(session_name))?;
        let result = {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            mutate(session)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                self.replace_session(session_name, previous_session)?;
                return Err(error);
            }
        };

        if let Err(error) = self.resize_terminals(session_name) {
            self.restore_session_after_resize_error(session_name, previous_session, &error)?;
            return Err(error);
        }

        self.synchronize_session_group_from(session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(session_name);

        Ok(result)
    }

    pub(crate) fn mutate_session_transfer_and_resize_terminals<T, C, F, M, R, A>(
        &mut self,
        session_name: &SessionName,
        mutate: F,
        move_runtime: M,
        rollback_runtime: R,
        finalize_model: A,
    ) -> Result<(T, C), RmuxError>
    where
        F: FnOnce(&mut Session) -> Result<T, RmuxError>,
        M: FnOnce(&mut Self, &T) -> Result<(), RmuxError>,
        R: FnOnce(&mut Self, &T) -> Result<(), RmuxError>,
        A: FnOnce(&mut Self, &T) -> Result<C, RmuxError>,
    {
        let snapshot = SessionTransferSnapshot::capture(self);
        let result = {
            let session = self
                .sessions
                .session_mut(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            mutate(session)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };

        if let Err(error) = move_runtime(self, &result) {
            snapshot.restore(self);
            return Err(error);
        }

        let committed = finalize_model(self, &result)
            .and_then(|committed| self.resize_terminals(session_name).map(|()| committed));
        let committed = match committed {
            Ok(committed) => committed,
            Err(error) => {
                let runtime_rollback = rollback_runtime(self, &result);
                snapshot.restore(self);
                let session_rollback =
                    self.resize_terminals(session_name)
                        .map_err(|rollback_error| {
                            RmuxError::Server(format!(
                        "failed to roll back session {session_name} after {error}: {rollback_error}"
                    ))
                        });
                if let Err(rollback_error) = runtime_rollback {
                    return Err(RmuxError::Server(format!(
                        "failed to roll back pane runtime transfer after {error}: {rollback_error}"
                    )));
                }
                session_rollback?;
                return Err(error);
            }
        };

        Ok((result, committed))
    }
}
