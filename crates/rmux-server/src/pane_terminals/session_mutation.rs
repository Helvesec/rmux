#[cfg(test)]
use crate::session_fixture::SessionSetup;
#[cfg(test)]
use crate::session_fixture::SessionSetupExt as _;
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(test)]
use super::pane_terminal_store::native_resize::WindowMutationResizeError;
use rmux_core::{
    AttachedWindowGeometry, EnvironmentStore, HookStore, ManagedSessionStore, OptionStore, PaneId,
    WindowId,
};
use rmux_proto::{PaneTarget, RmuxError, SessionId, SessionName, TerminalSize, WindowTarget};

use super::{
    session_not_found, AppliedWindowResizeQueue, HandlerState, PaneExitObservation,
    PaneLifecycleState, WindowLinkGroup, WindowLinkSlot,
};

pub(crate) struct SessionTransferSnapshot {
    sessions: rmux_core::ManagedStructureCheckpoint,
    options: OptionStore,
    environment: EnvironmentStore,
    hooks: HookStore,
    pane_lifecycle: HashMap<PaneId, PaneLifecycleState>,
    attached_terminal_geometry: HashMap<SessionName, rmux_proto::TerminalGeometry>,
    dead_panes: HashMap<SessionName, HashMap<PaneId, PaneExitObservation>>,
    kept_dead_pane_preparations: HashMap<SessionName, HashMap<PaneId, u64>>,
    auto_named_windows: HashSet<(SessionName, u32)>,
    window_link_groups: HashMap<u64, WindowLinkGroup>,
    window_link_slots: HashMap<WindowLinkSlot, u64>,
    window_link_occurrences: HashMap<WindowLinkSlot, super::WindowLinkOccurrenceId>,
    next_window_link_group_id: u64,
    next_window_link_occurrence_id: u64,
    applied_window_resizes: AppliedWindowResizeQueue,
}

struct WindowGeometryBefore {
    window_id: WindowId,
    preferred_target: WindowTarget,
    preferred_sessions: Vec<SessionName>,
    size: TerminalSize,
}

struct WindowGeometrySnapshot {
    windows: Vec<WindowGeometryBefore>,
}

#[derive(Debug, Clone)]
pub(super) struct PaneTransferGeometryContext {
    source: WindowTarget,
    target: WindowTarget,
}

impl PaneTransferGeometryContext {
    pub(super) fn new(source: &PaneTarget, target: &PaneTarget) -> Self {
        Self {
            source: WindowTarget::with_window(source.session_name().clone(), source.window_index()),
            target: WindowTarget::with_window(target.session_name().clone(), target.window_index()),
        }
    }

    fn preferred_sessions(
        &self,
        window_id: WindowId,
        source_window_id: Option<WindowId>,
    ) -> Vec<SessionName> {
        let (first, second) = if source_window_id == Some(window_id) {
            (self.source.session_name(), self.target.session_name())
        } else {
            (self.target.session_name(), self.source.session_name())
        };
        let mut sessions = vec![first.clone()];
        if second != first {
            sessions.push(second.clone());
        }
        sessions
    }
}

impl SessionTransferSnapshot {
    pub(crate) fn capture(state: &HandlerState, names: &[SessionName]) -> Result<Self, RmuxError> {
        Ok(Self {
            sessions: state.sessions.checkpoint_structure(names)?,
            options: state.options.clone(),
            environment: state.environment.clone(),
            hooks: state.hooks.clone(),
            pane_lifecycle: state.pane_lifecycle.clone(),
            attached_terminal_geometry: state.attached_terminal_geometry.clone(),
            dead_panes: state.dead_panes.clone(),
            kept_dead_pane_preparations: state.kept_dead_pane_preparations.clone(),
            auto_named_windows: state.auto_named_windows.clone(),
            window_link_groups: state.window_link_groups.clone(),
            window_link_slots: state.window_link_slots.clone(),
            window_link_occurrences: state.window_link_occurrences.clone(),
            next_window_link_group_id: state.next_window_link_group_id,
            next_window_link_occurrence_id: state.next_window_link_occurrence_id,
            applied_window_resizes: state.applied_window_resizes.clone(),
        })
    }

    pub(crate) fn retain_removed(
        &mut self,
        windows: impl IntoIterator<Item = Box<rmux_core::Window>>,
    ) {
        self.sessions.retain_removed(windows);
    }

    pub(crate) fn transfer_removed_to(
        &mut self,
        outer: &mut rmux_core::ManagedStructureCheckpoint,
    ) {
        outer.retain_removed(self.sessions.take_removed());
    }

    pub(crate) fn retain_transfer(&mut self, transfer: rmux_core::ManagedPaneTransfer) {
        self.retain_removed(transfer.change.removed_windows);
        if let Some(removal) = transfer.destroyed_window {
            self.retain_removed(removal.window);
        }
    }

    pub(crate) fn restore(self, state: &mut HandlerState) -> Result<(), RmuxError> {
        state.sessions.restore_structure_checkpoint(self.sessions)?;
        state.options = self.options;
        state.environment = self.environment;
        state.hooks = self.hooks;
        state.pane_lifecycle = self.pane_lifecycle;
        state.attached_terminal_geometry = self.attached_terminal_geometry;
        state.dead_panes = self.dead_panes;
        state.kept_dead_pane_preparations = self.kept_dead_pane_preparations;
        state.auto_named_windows = self.auto_named_windows;
        state.window_link_groups = self.window_link_groups;
        state.window_link_slots = self.window_link_slots;
        state.window_link_occurrences = self.window_link_occurrences;
        state.next_window_link_group_id = self.next_window_link_group_id;
        state.next_window_link_occurrence_id = self.next_window_link_occurrence_id;
        // A rolled-back geometry change owes no notification: restoring the
        // session model must also retract the resizes it undid.
        state.applied_window_resizes = self.applied_window_resizes;
        Ok(())
    }
}

impl WindowGeometrySnapshot {
    fn capture_all(state: &HandlerState, context: &PaneTransferGeometryContext) -> Self {
        let source_window_id = window_id_at(state, &context.source);
        let target_window_id = window_id_at(state, &context.target);
        let mut aliases_by_window = BTreeMap::<WindowId, Vec<(WindowTarget, TerminalSize)>>::new();
        for (session_name, session) in state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            for (window_index, window) in session.windows() {
                aliases_by_window.entry(window.id()).or_default().push((
                    WindowTarget::with_window(session_name.clone(), window_index),
                    window.size(),
                ));
            }
        }

        for aliases in aliases_by_window.values_mut() {
            aliases.sort_by(|(left, _), (right, _)| {
                (left.session_name().as_str(), left.window_index())
                    .cmp(&(right.session_name().as_str(), right.window_index()))
            });
        }

        let mut seen = HashSet::new();
        let mut ordered_window_ids = Vec::with_capacity(aliases_by_window.len());
        for window_id in [source_window_id, target_window_id].into_iter().flatten() {
            if aliases_by_window.contains_key(&window_id) && seen.insert(window_id) {
                ordered_window_ids.push(window_id);
            }
        }
        for &window_id in aliases_by_window.keys() {
            if seen.insert(window_id) {
                ordered_window_ids.push(window_id);
            }
        }

        let windows = ordered_window_ids
            .into_iter()
            .filter_map(|window_id| {
                let aliases = aliases_by_window.remove(&window_id)?;
                let preferred_sessions = context.preferred_sessions(window_id, source_window_id);
                let requested_target = if source_window_id == Some(window_id) {
                    Some(&context.source)
                } else if target_window_id == Some(window_id) {
                    Some(&context.target)
                } else {
                    None
                };
                let (preferred_target, size) =
                    preferred_alias(&aliases, requested_target, &preferred_sessions)?;
                Some(WindowGeometryBefore {
                    window_id,
                    preferred_target,
                    preferred_sessions,
                    size,
                })
            })
            .collect();
        Self { windows }
    }

    fn record_changes(self, state: &mut HandlerState) {
        for before in self.windows {
            let Some((target, size)) = before.resolve_current(state) else {
                continue;
            };
            if size != before.size {
                state.record_applied_window_resize(target);
            }
        }
    }
}

impl WindowGeometryBefore {
    fn resolve_current(&self, state: &HandlerState) -> Option<(WindowTarget, TerminalSize)> {
        if let Some(window) = state
            .sessions
            .session(self.preferred_target.session_name())
            .and_then(|session| session.window_at(self.preferred_target.window_index()))
            .filter(|window| window.id() == self.window_id)
        {
            return Some((self.preferred_target.clone(), window.size()));
        }

        for session_name in &self.preferred_sessions {
            let Some(session) = state.sessions.session(session_name) else {
                continue;
            };
            if let Some((window_index, window)) = session
                .windows()
                .find(|(_, window)| window.id() == self.window_id)
            {
                return Some((
                    WindowTarget::with_window(session_name.clone(), window_index),
                    window.size(),
                ));
            }
        }

        let mut fallback = None;
        for (session_name, session) in state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
        {
            for (window_index, window) in session.windows() {
                if window.id() != self.window_id {
                    continue;
                }
                let is_better = fallback.as_ref().is_none_or(
                    |(best_session, best_index, _): &(SessionName, u32, TerminalSize)| {
                        (session_name.as_str(), window_index) < (best_session.as_str(), *best_index)
                    },
                );
                if is_better {
                    fallback = Some((session_name.clone(), window_index, window.size()));
                }
            }
        }
        fallback.map(|(session_name, window_index, size)| {
            (WindowTarget::with_window(session_name, window_index), size)
        })
    }
}

fn window_id_at(state: &HandlerState, target: &WindowTarget) -> Option<WindowId> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(rmux_core::Window::id)
}

fn preferred_alias(
    aliases: &[(WindowTarget, TerminalSize)],
    requested_target: Option<&WindowTarget>,
    preferred_sessions: &[SessionName],
) -> Option<(WindowTarget, TerminalSize)> {
    if let Some(requested_target) = requested_target {
        if let Some((target, size)) = aliases
            .iter()
            .find(|(target, _)| target == requested_target)
        {
            return Some((target.clone(), *size));
        }
    }
    for session_name in preferred_sessions {
        if let Some((target, size)) = aliases
            .iter()
            .find(|(target, _)| target.session_name() == session_name)
        {
            return Some((target.clone(), *size));
        }
    }
    aliases
        .first()
        .map(|(target, size)| (target.clone(), *size))
}

impl HandlerState {
    /// Runs one join/move mutation, then records every surviving stable
    /// window identity whose content size changed.
    ///
    /// Both public join/move entry points pass through this boundary and the
    /// snapshot covers every existing window, so callers cannot silently omit
    /// a sibling target. The typed context controls deterministic alias and
    /// publication preference only; it never limits snapshot coverage. The
    /// comparison runs after both success and error returns. Transactional
    /// errors restore their model snapshot and compare equal; a partially
    /// committed error still owes the same notification as a successful write.
    pub(super) fn mutate_join_or_move_and_record_window_geometry_changes<T, F>(
        &mut self,
        context: PaneTransferGeometryContext,
        mutate: F,
    ) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        let geometry_before = WindowGeometrySnapshot::capture_all(self, &context);
        let result = mutate(self);
        geometry_before.record_changes(self);
        result
    }

    pub(crate) fn mutate_attached_window_geometry_and_resize_terminal<T, F>(
        &mut self,
        target: &WindowTarget,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(AttachedWindowGeometry<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            target.session_name(),
            target.window_index(),
            |store| mutate(store.attached_window_geometry_mut(target)?),
            |_| true,
        )
        .map(|(result, _)| result)
        .map_err(RmuxError::from)
    }

    /// Keeps attach recency and geometry in one window transaction while the
    /// callback receives no access to unrelated session state.
    pub(crate) fn mutate_touched_attached_window_geometry_and_resize_terminal<T, F>(
        &mut self,
        target: &WindowTarget,
        expected_session_id: SessionId,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(AttachedWindowGeometry<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            target.session_name(),
            target.window_index(),
            |store| {
                if store
                    .session(target.session_name())
                    .is_none_or(|session| session.id() != expected_session_id)
                {
                    return Err(session_not_found(target.session_name()));
                }
                mutate(store.touched_attached_window_geometry_mut(target)?)
            },
            |_| true,
        )
        .map(|(result, _)| result)
        .map_err(RmuxError::from)
    }

    #[cfg(test)]
    pub(crate) fn mutate_session_and_resize_window_terminal<T, F>(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut SessionSetup<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_session_and_resize_window_terminal_with_family(
            session_name,
            window_index,
            mutate,
        )
        .map(|(result, _)| result)
    }

    #[cfg(test)]
    pub(crate) fn mutate_session_and_resize_window_terminal_with_family<T, F>(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
    ) -> Result<(T, Vec<SessionName>), RmuxError>
    where
        F: FnOnce(&mut SessionSetup<'_>) -> Result<T, RmuxError>,
    {
        self.mutate_session_and_resize_window_terminal_with_family_if(
            session_name,
            window_index,
            mutate,
            |_| true,
        )
        .map_err(RmuxError::from)
    }

    /// Runs borrowed fixture setup inside the shared window transaction.
    #[cfg(test)]
    pub(crate) fn mutate_session_and_resize_window_terminal_with_family_if<T, F, P>(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        mutate: F,
        should_resize: P,
    ) -> Result<(T, Vec<SessionName>), WindowMutationResizeError>
    where
        F: FnOnce(&mut SessionSetup<'_>) -> Result<T, RmuxError>,
        P: FnOnce(&T) -> bool,
    {
        self.mutate_store_and_resize_window_terminal_with_family_if(
            session_name,
            window_index,
            |store| {
                mutate(
                    &mut store
                        .session_setup(session_name)
                        .ok_or_else(|| session_not_found(session_name))?,
                )
            },
            should_resize,
        )
    }

    #[cfg(test)]
    pub(crate) fn mutate_session_and_resize_terminals<T, F>(
        &mut self,
        session_name: &SessionName,
        mutate: F,
    ) -> Result<T, RmuxError>
    where
        F: FnOnce(&mut SessionSetup<'_>) -> Result<T, RmuxError>,
    {
        let snapshot = SessionTransferSnapshot::capture(self, std::slice::from_ref(session_name))?;
        let result = mutate(
            &mut self
                .sessions
                .session_setup(session_name)
                .ok_or_else(|| session_not_found(session_name))?,
        );
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                snapshot.restore(self)?;
                return Err(error);
            }
        };
        if let Err(error) = self.resize_terminals(session_name) {
            snapshot.restore(self)?;
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
        F: FnOnce(&mut ManagedSessionStore, &mut SessionTransferSnapshot) -> Result<T, RmuxError>,
        M: FnOnce(&mut Self, &T) -> Result<(), RmuxError>,
        R: FnOnce(&mut Self, &T) -> Result<(), RmuxError>,
        A: FnOnce(&mut Self, &T) -> Result<C, RmuxError>,
    {
        let mut snapshot =
            SessionTransferSnapshot::capture(self, std::slice::from_ref(session_name))?;
        let result = mutate(&mut self.sessions, &mut snapshot);
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                snapshot.restore(self)?;
                return Err(error);
            }
        };

        if let Err(error) = move_runtime(self, &result) {
            snapshot.restore(self)?;
            return Err(error);
        }

        let committed = finalize_model(self, &result)
            .and_then(|committed| self.resize_terminals(session_name).map(|()| committed));
        let committed = match committed {
            Ok(committed) => committed,
            Err(error) => {
                let runtime_rollback = rollback_runtime(self, &result);
                snapshot.restore(self)?;
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

#[cfg(test)]
#[path = "session_mutation_tests.rs"]
mod tests;
