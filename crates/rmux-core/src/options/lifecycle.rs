//! Option overrides that follow the lifecycle of their targets: removal,
//! renaming, copies, swaps, moves and index renumbering of sessions, windows
//! and panes.

use std::collections::{BTreeMap, HashMap, HashSet};

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, PaneTarget, RmuxError, SessionName, WindowTarget};

use super::storage::OptionNode;
use super::{OptionStore, PaneOptions};

impl OptionStore {
    /// Removes all option overrides owned by the given session.
    pub fn remove_session(
        &mut self,
        session_name: &SessionName,
    ) -> Option<HashMap<OptionName, String>> {
        self.windows
            .retain(|target, _| target.session_name() != session_name);
        self.panes
            .retain(|target, _| target.session_name() != session_name);
        self.sessions
            .remove(session_name)
            .map(|node| node.into_known_values())
    }

    /// Rekeys all option overrides owned by the given session.
    pub fn rename_session(
        &mut self,
        session_name: &SessionName,
        new_name: SessionName,
    ) -> Result<(), RmuxError> {
        let mut renamed_sessions = HashMap::with_capacity(self.sessions.len());
        for (name, values) in &self.sessions {
            let next_name = if name == session_name {
                new_name.clone()
            } else {
                name.clone()
            };
            if renamed_sessions
                .insert(next_name.clone(), values.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "session options already exist for session {next_name}"
                )));
            }
        }

        let mut renamed_windows = HashMap::with_capacity(self.windows.len());
        for (target, values) in &self.windows {
            let next_target = if target.session_name() == session_name {
                WindowTarget::with_window(new_name.clone(), target.window_index())
            } else {
                target.clone()
            };
            if renamed_windows
                .insert(next_target.clone(), values.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "window options already exist for {next_target}"
                )));
            }
        }

        let mut renamed_panes = HashMap::with_capacity(self.panes.len());
        for (target, values) in &self.panes {
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
                .insert(next_target.clone(), values.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "pane options already exist for {next_target}"
                )));
            }
        }

        self.sessions = renamed_sessions;
        self.windows = renamed_windows;
        self.panes = renamed_panes;
        Ok(())
    }

    /// Removes all window and pane option overrides owned by the given window.
    pub fn remove_window(&mut self, target: &WindowTarget) -> Option<HashMap<OptionName, String>> {
        self.panes.retain(|pane_target, _| {
            pane_target.session_name() != target.session_name()
                || pane_target.window_index() != target.window_index()
        });
        self.windows
            .remove(target)
            .map(OptionNode::into_known_values)
    }

    /// Copies exact window and pane overrides from one winlink slot to another.
    pub fn copy_window_overrides(&mut self, source: &WindowTarget, target: &WindowTarget) {
        if let Some(source_window) = self.windows.get(source).cloned() {
            self.windows.insert(
                target.clone(),
                source_window.with_scope(OptionScopeSelector::Window(target.clone())),
            );
        } else {
            self.windows.remove(target);
        }

        self.panes.retain(|pane_target, _| {
            pane_target.session_name() != target.session_name()
                || pane_target.window_index() != target.window_index()
        });

        let source_panes = self
            .panes
            .iter()
            .filter(|(pane_target, _)| {
                pane_target.session_name() == source.session_name()
                    && pane_target.window_index() == source.window_index()
            })
            .map(|(pane_target, node)| {
                let target_pane = PaneTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                    pane_target.pane_index(),
                );
                (
                    target_pane.clone(),
                    node.clone()
                        .with_scope(OptionScopeSelector::Pane(target_pane)),
                )
            })
            .collect::<Vec<_>>();

        self.panes.extend(source_panes);
    }

    /// Swaps exact window and pane overrides between two winlink slots.
    pub fn swap_window_overrides(&mut self, source: &WindowTarget, target: &WindowTarget) {
        if source == target {
            return;
        }

        let source_window = self.windows.remove(source);
        let target_window = self.windows.remove(target);
        if let Some(node) = source_window {
            self.windows.insert(
                target.clone(),
                node.with_scope(OptionScopeSelector::Window(target.clone())),
            );
        }
        if let Some(node) = target_window {
            self.windows.insert(
                source.clone(),
                node.with_scope(OptionScopeSelector::Window(source.clone())),
            );
        }

        let source_panes = remove_window_pane_options(&mut self.panes, source);
        let target_panes = remove_window_pane_options(&mut self.panes, target);
        self.panes
            .extend(rekey_pane_options(source_panes, source, target));
        self.panes
            .extend(rekey_pane_options(target_panes, target, source));
    }

    /// Moves exact window and pane overrides from one winlink slot to another.
    pub fn move_window_overrides(&mut self, source: &WindowTarget, target: &WindowTarget) {
        if source == target {
            return;
        }

        let source_window = self.windows.remove(source);
        let _ = self.windows.remove(target);
        if let Some(node) = source_window {
            self.windows.insert(
                target.clone(),
                node.with_scope(OptionScopeSelector::Window(target.clone())),
            );
        }

        let source_panes = remove_window_pane_options(&mut self.panes, source);
        let _ = remove_window_pane_options(&mut self.panes, target);
        self.panes
            .extend(rekey_pane_options(source_panes, source, target));
    }

    /// Removes all pane option overrides owned by the given pane.
    pub fn remove_pane(&mut self, target: &PaneTarget) -> Option<HashMap<OptionName, String>> {
        self.panes.remove(target).map(OptionNode::into_known_values)
    }

    /// Copies exact pane-local overrides between two aliases of one pane.
    pub fn copy_pane_overrides(&mut self, source: &PaneTarget, target: &PaneTarget) {
        if source == target {
            return;
        }
        if let Some(source_pane) = self.panes.get(source).cloned() {
            self.panes.insert(
                target.clone(),
                source_pane.with_scope(OptionScopeSelector::Pane(target.clone())),
            );
        } else {
            self.panes.remove(target);
        }
    }

    /// Rekeys pane-local overrides after pane indices change within one window.
    pub fn remap_pane_indices(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<(), RmuxError> {
        if index_map.is_empty() {
            return Ok(());
        }

        let mut remapped_panes = HashMap::with_capacity(self.panes.len());
        for (target, values) in &self.panes {
            let next_target =
                remapped_pane_index_target(target, session_name, window_index, index_map);
            if remapped_panes
                .insert(
                    next_target.clone(),
                    values
                        .clone()
                        .with_scope(OptionScopeSelector::Pane(next_target.clone())),
                )
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "pane options already exist for {next_target}"
                )));
            }
        }

        self.panes = remapped_panes;
        Ok(())
    }

    /// Moves exact pane-local overrides from one pane slot to another.
    pub fn transfer_pane_overrides(&mut self, source: &PaneTarget, target: &PaneTarget) {
        self.rekey_pane_overrides(&[(source.clone(), Some(target.clone()))])
            .expect("single pane override transfer cannot collide");
    }

    /// Swaps exact pane-local overrides between two pane slots.
    pub fn swap_pane_overrides(&mut self, source: &PaneTarget, target: &PaneTarget) {
        if source == target {
            return;
        }

        self.rekey_pane_overrides(&[
            (source.clone(), Some(target.clone())),
            (target.clone(), Some(source.clone())),
        ])
        .expect("two-way pane override swap cannot collide");
    }

    /// Atomically rekeys or removes exact pane-local overrides.
    pub fn rekey_pane_overrides(
        &mut self,
        mappings: &[(PaneTarget, Option<PaneTarget>)],
    ) -> Result<(), RmuxError> {
        if mappings.is_empty() {
            return Ok(());
        }

        let mut targets = HashSet::new();
        for (_, target) in mappings {
            if let Some(target) = target {
                if !targets.insert(target.clone()) {
                    return Err(RmuxError::Server(format!(
                        "pane options remap has duplicate destination {target}"
                    )));
                }
            }
        }

        let mut removed = Vec::with_capacity(mappings.len());
        for (source, _) in mappings {
            if let Some(node) = self.panes.remove(source) {
                removed.push((source.clone(), node));
            }
        }

        for (_, target) in mappings {
            if let Some(target) = target {
                let _ = self.panes.remove(target);
            }
        }

        for (source, node) in removed {
            let Some((_, Some(target))) = mappings
                .iter()
                .find(|(mapped_source, _)| *mapped_source == source)
            else {
                continue;
            };
            if self
                .panes
                .insert(
                    target.clone(),
                    node.with_scope(OptionScopeSelector::Pane(target.clone())),
                )
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "pane options already exist for {target}"
                )));
            }
        }
        Ok(())
    }

    /// Rekeys window and pane option overrides after a session window reindex.
    pub fn remap_session_window_indices(
        &mut self,
        session_name: &SessionName,
        index_map: &BTreeMap<u32, u32>,
    ) -> Result<(), RmuxError> {
        let mut remapped_windows = HashMap::with_capacity(self.windows.len());
        for (target, values) in &self.windows {
            let next_target = remapped_window_target(target, session_name, index_map);
            if remapped_windows
                .insert(next_target.clone(), values.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "window options already exist for {next_target}"
                )));
            }
        }

        let mut remapped_panes = HashMap::with_capacity(self.panes.len());
        for (target, values) in &self.panes {
            let next_target = remapped_pane_target(target, session_name, index_map);
            if remapped_panes
                .insert(next_target.clone(), values.clone())
                .is_some()
            {
                return Err(RmuxError::Server(format!(
                    "pane options already exist for {next_target}"
                )));
            }
        }

        self.windows = remapped_windows;
        self.panes = remapped_panes;
        Ok(())
    }
}

fn remove_window_pane_options(
    panes: &mut PaneOptions,
    window: &WindowTarget,
) -> Vec<(PaneTarget, OptionNode)> {
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
        .filter_map(|pane_target| panes.remove(&pane_target).map(|node| (pane_target, node)))
        .collect()
}

fn rekey_pane_options(
    panes: Vec<(PaneTarget, OptionNode)>,
    source: &WindowTarget,
    target: &WindowTarget,
) -> Vec<(PaneTarget, OptionNode)> {
    panes
        .into_iter()
        .map(move |(pane_target, node)| {
            let next_target = PaneTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
                pane_target.pane_index(),
            );
            let next_node = node.with_scope(OptionScopeSelector::Pane(next_target.clone()));
            debug_assert_eq!(pane_target.session_name(), source.session_name());
            debug_assert_eq!(pane_target.window_index(), source.window_index());
            (next_target, next_node)
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

fn remapped_pane_index_target(
    target: &PaneTarget,
    session_name: &SessionName,
    window_index: u32,
    index_map: &BTreeMap<u32, u32>,
) -> PaneTarget {
    if target.session_name() != session_name || target.window_index() != window_index {
        return target.clone();
    }
    index_map.get(&target.pane_index()).copied().map_or_else(
        || target.clone(),
        |pane_index| PaneTarget::with_window(session_name.clone(), window_index, pane_index),
    )
}
