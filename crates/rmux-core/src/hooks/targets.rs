use std::collections::{BTreeMap, HashMap};

use rmux_proto::{PaneTarget, RmuxError, SessionName, WindowTarget};

use super::{HookBindings, HookStore};

impl HookStore {
    /// Removes all hooks owned by the given session.
    pub fn remove_session(&mut self, session_name: &SessionName) -> bool {
        let mut removed = self.sessions.remove(session_name).is_some();
        self.window_aliases
            .retain(|target, _| target.session_name() != session_name);
        self.pane_aliases
            .retain(|target, _| target.session_name() != session_name);
        self.windows.retain(|target, _| {
            let keep = target.session_name() != session_name;
            removed |= !keep;
            keep
        });
        self.panes.retain(|target, _| {
            let keep = target.session_name() != session_name;
            removed |= !keep;
            keep
        });
        removed
    }

    /// Removes all hooks owned by the given window and its panes.
    pub fn remove_window(&mut self, target: &WindowTarget) -> bool {
        let mut removed = self.windows.remove(target).is_some();
        self.window_aliases.remove(target);
        self.pane_aliases.retain(|pane_target, _| {
            pane_target.session_name() != target.session_name()
                || pane_target.window_index() != target.window_index()
        });
        self.panes.retain(|pane_target, _| {
            let keep = pane_target.session_name() != target.session_name()
                || pane_target.window_index() != target.window_index();
            removed |= !keep;
            keep
        });
        removed
    }

    /// Swaps window and pane hooks between two winlink slots.
    pub fn swap_window_hooks(&mut self, source: &WindowTarget, target: &WindowTarget) {
        if source == target {
            return;
        }

        let source_window = self.windows.remove(source);
        let target_window = self.windows.remove(target);
        if let Some(bindings) = source_window {
            self.windows.insert(target.clone(), bindings);
        }
        if let Some(bindings) = target_window {
            self.windows.insert(source.clone(), bindings);
        }

        let source_panes = remove_window_pane_hooks(&mut self.panes, source);
        let target_panes = remove_window_pane_hooks(&mut self.panes, target);
        self.panes
            .extend(rekey_pane_hooks(source_panes, source, target));
        self.panes
            .extend(rekey_pane_hooks(target_panes, target, source));
    }

    /// Moves window and pane hooks from one winlink slot to another.
    pub fn move_window_hooks(&mut self, source: &WindowTarget, target: &WindowTarget) {
        if source == target {
            return;
        }

        let source_window = self.windows.remove(source);
        let _ = self.windows.remove(target);
        if let Some(bindings) = source_window {
            self.windows.insert(target.clone(), bindings);
        }

        let source_panes = remove_window_pane_hooks(&mut self.panes, source);
        let _ = remove_window_pane_hooks(&mut self.panes, target);
        self.panes
            .extend(rekey_pane_hooks(source_panes, source, target));
    }

    /// Removes all hooks owned by the given pane.
    pub fn remove_pane(&mut self, target: &PaneTarget) -> bool {
        self.pane_aliases.remove(target);
        self.panes.remove(target).is_some()
    }

    /// Rekeys window and pane hooks after a session window reindex.
    pub fn remap_session_window_indices(
        &mut self,
        session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<(), RmuxError> {
        let mut remapped_windows = HashMap::with_capacity(self.windows.len());
        for (target, bindings) in &self.windows {
            let next_target = remapped_window_target(target, session_name, index_map);
            if remapped_windows
                .insert(next_target.clone(), bindings.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "hooks already exist for {next_target}"
                )));
            }
        }

        let mut remapped_panes = HashMap::with_capacity(self.panes.len());
        for (target, bindings) in &self.panes {
            let next_target = remapped_pane_target(target, session_name, index_map);
            if remapped_panes
                .insert(next_target.clone(), bindings.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "hooks already exist for {next_target}"
                )));
            }
        }

        self.windows = remapped_windows;
        self.panes = remapped_panes;
        Ok(())
    }

    /// Rekeys all hooks owned by the given session.
    pub fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: SessionName,
    ) -> Result<(), RmuxError> {
        let mut renamed_sessions = HashMap::with_capacity(self.sessions.len());
        for (name, bindings) in &self.sessions {
            let next_name = if name == session_name {
                new_name.clone()
            } else {
                name.clone()
            };
            if renamed_sessions
                .insert(next_name.clone(), bindings.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "hooks already exist for session {next_name}"
                )));
            }
        }

        let mut renamed_windows = HashMap::with_capacity(self.windows.len());
        for (target, bindings) in &self.windows {
            let next_target = if target.session_name() == session_name {
                WindowTarget::with_window(new_name.clone(), target.window_index())
            } else {
                target.clone()
            };
            if renamed_windows
                .insert(next_target.clone(), bindings.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "hooks already exist for {next_target}"
                )));
            }
        }

        let mut renamed_panes = HashMap::with_capacity(self.panes.len());
        for (target, bindings) in &self.panes {
            let next_target = if target.session_name() == session_name {
                PaneTarget::with_window(
                    new_name.clone(),
                    target.window_index(),
                    target.pane_index(),
                )
            } else {
                target.clone()
            };
            if renamed_panes
                .insert(next_target.clone(), bindings.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "hooks already exist for {next_target}"
                )));
            }
        }

        self.sessions = renamed_sessions;
        self.windows = renamed_windows;
        self.panes = renamed_panes;
        Ok(())
    }
}

fn remove_window_pane_hooks(
    panes: &mut HashMap<PaneTarget, HookBindings>,
    window: &WindowTarget,
) -> Vec<(PaneTarget, HookBindings)> {
    let pane_targets = panes
        .keys()
        .filter(|pane_target| {
            pane_target.session_name() == window.session_name()
                && pane_target.window_index() == window.window_index()
        })
        .cloned()
        .collect::<Vec<_>>();
    pane_targets
        .into_iter()
        .filter_map(|pane_target| {
            panes
                .remove(&pane_target)
                .map(|bindings| (pane_target, bindings))
        })
        .collect()
}

fn rekey_pane_hooks(
    panes: Vec<(PaneTarget, HookBindings)>,
    source: &WindowTarget,
    target: &WindowTarget,
) -> Vec<(PaneTarget, HookBindings)> {
    panes
        .into_iter()
        .map(move |(pane_target, bindings)| {
            debug_assert_eq!(pane_target.session_name(), source.session_name());
            debug_assert_eq!(pane_target.window_index(), source.window_index());
            (
                PaneTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                    pane_target.pane_index(),
                ),
                bindings,
            )
        })
        .collect()
}

fn remapped_window_target(
    target: &WindowTarget,
    session_name: &SessionName,
    index_map: &BTreeMap<u32, u32>,
) -> WindowTarget {
    if target.session_name() != session_name {
        return target.clone();
    }
    index_map.get(&target.window_index()).copied().map_or_else(
        || target.clone(),
        |window_index| WindowTarget::with_window(session_name.clone(), window_index),
    )
}

fn remapped_pane_target(
    target: &PaneTarget,
    session_name: &SessionName,
    index_map: &BTreeMap<u32, u32>,
) -> PaneTarget {
    if target.session_name() != session_name {
        return target.clone();
    }
    index_map.get(&target.window_index()).copied().map_or_else(
        || target.clone(),
        |window_index| {
            PaneTarget::with_window(session_name.clone(), window_index, target.pane_index())
        },
    )
}
