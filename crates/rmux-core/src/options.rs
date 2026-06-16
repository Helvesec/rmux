use std::collections::{BTreeMap, HashMap};

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName, SetOptionMode, WindowTarget,
};

#[path = "options/access.rs"]
mod access;
#[path = "options/mutation.rs"]
mod mutation;
#[path = "options/registry.rs"]
mod registry;
#[path = "options/render.rs"]
mod render;
#[path = "options/scope.rs"]
mod scope;
#[path = "options/show.rs"]
mod show;
#[path = "options/storage.rs"]
mod storage;

use mutation::{
    apply_array_mutation, build_mutation_outcome, is_global_scope, legacy_scope_for_option,
    normalize_scalar_value,
};
pub use mutation::{validate_option_mutation, validate_option_name_mutation};
pub use registry::{
    option_affects_alerts, option_affects_rendering, option_name_by_name, resolve_option_name,
    OptionQuery,
};
use registry::{option_metadata, OptionChangeMask, OptionValueType};
pub use scope::default_global_scope_for_option_name;
use storage::{OptionEntry, OptionNode};

/// Option rendering mode for `show-options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShowOptionsMode {
    /// Render the fully resolved view for each known option.
    Resolved,
    /// Render the resolved view and mark inherited values with `*`, matching `show-options -A`.
    ResolvedWithInheritanceMarkers,
    /// Render only entries explicitly present in the selected tree.
    Explicit,
}

/// A server-visible option mutation side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionNotification {
    /// The canonical option name.
    pub name: String,
    /// The exact scope that was mutated.
    pub scope: OptionScopeSelector,
    /// The effect bitmask associated with the option.
    pub effects: OptionChangeMask,
}

/// Outcome for a successful option mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionMutationOutcome {
    /// The canonical option name.
    pub name: String,
    /// The known wire option, when the option is part of the closed V1 registry.
    pub known_option: Option<OptionName>,
    /// Side effects the server may react to.
    pub notifications: Vec<OptionNotification>,
}

type SessionOptions = HashMap<SessionName, OptionNode>;
type WindowOptions = HashMap<WindowTarget, OptionNode>;
type PaneOptions = HashMap<PaneTarget, OptionNode>;

/// In-memory storage for supported RMUX option values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OptionStore {
    server_global: OptionNode,
    session_global: OptionNode,
    window_global: OptionNode,
    sessions: SessionOptions,
    windows: WindowOptions,
    panes: PaneOptions,
}

impl OptionStore {
    /// Creates an empty option store with no explicit overrides.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether no explicit option overrides are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.server_global.is_empty()
            && self.session_global.is_empty()
            && self.window_global.is_empty()
            && self.sessions.values().all(OptionNode::is_empty)
            && self.windows.values().all(OptionNode::is_empty)
            && self.panes.values().all(OptionNode::is_empty)
    }

    /// Applies a mutation for a known legacy option.
    pub fn set(
        &mut self,
        scope: ScopeSelector,
        option: OptionName,
        value: String,
        mode: SetOptionMode,
    ) -> Result<OptionMutationOutcome, RmuxError> {
        let explicit_scope = legacy_scope_for_option(option, &scope);
        self.set_by_name(
            explicit_scope,
            option_metadata(option).name(),
            Some(value),
            mode,
            false,
            false,
            false,
        )
    }

    /// Applies a mutation using a tmux-style string option name.
    #[allow(clippy::too_many_arguments)]
    pub fn set_by_name(
        &mut self,
        scope: OptionScopeSelector,
        name: &str,
        value: Option<String>,
        mode: SetOptionMode,
        only_if_unset: bool,
        unset: bool,
        unset_pane_overrides: bool,
    ) -> Result<OptionMutationOutcome, RmuxError> {
        if unset_pane_overrides && !matches!(scope, OptionScopeSelector::Window(_)) {
            return Err(RmuxError::InvalidSetOption(
                "unset pane overrides only supports window scope".to_owned(),
            ));
        }

        let query = validate_option_name_mutation(name, &scope, mode, value.as_deref(), unset)?;

        if unset_pane_overrides {
            self.unset_window_pane_overrides(&scope, query.canonical_name());
        }

        if unset {
            self.unset_query(scope, &query, only_if_unset)
        } else {
            self.set_query(scope, &query, value.as_deref(), mode, only_if_unset)
        }
    }

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

    fn set_query(
        &mut self,
        scope: OptionScopeSelector,
        query: &OptionQuery,
        value: Option<&str>,
        mode: SetOptionMode,
        only_if_unset: bool,
    ) -> Result<OptionMutationOutcome, RmuxError> {
        let effective_before = self
            .effective_value_for_scope(&scope, query)
            .or_else(|| self.default_value_as_string(query));
        let explicit_before = self.explicit_value_for_scope(&scope, query);
        let default_entry = self.default_entry_for_scope(query, scope.clone());
        let node = self.node_for_exact_scope_mut(&scope);
        if only_if_unset && explicit_before.is_some() {
            return Err(RmuxError::InvalidSetOption(format!(
                "{} is already set",
                query.canonical_name()
            )));
        }

        if query.is_array()
            && mode == SetOptionMode::Append
            && query.index().is_none()
            && value.unwrap_or_default().is_empty()
        {
            return Ok(build_mutation_outcome(query, scope));
        }

        if query.is_array() {
            let entry = node
                .entries
                .entry(query.canonical_name().to_owned())
                .or_insert_with(|| {
                    if is_global_scope(&scope) {
                        default_entry.unwrap_or_else(|| {
                            OptionEntry::new_empty_array(
                                query.canonical_name(),
                                query.known_option(),
                                scope.clone(),
                                query.value_type(),
                            )
                        })
                    } else {
                        OptionEntry::new_empty_array(
                            query.canonical_name(),
                            query.known_option(),
                            scope.clone(),
                            query.value_type(),
                        )
                    }
                });
            apply_array_mutation(
                entry,
                query,
                value.unwrap_or_default(),
                mode,
                explicit_before.as_deref(),
            )?;
        } else {
            let current = match (query.value_type(), mode) {
                (OptionValueType::String, SetOptionMode::Append) => {
                    if explicit_before.is_some() || is_global_scope(&scope) {
                        explicit_before.clone().or_else(|| effective_before.clone())
                    } else {
                        None
                    }
                }
                (OptionValueType::String, SetOptionMode::Replace) => None,
                _ => effective_before.clone(),
            };
            let normalized = normalize_scalar_value(query, value, current.as_deref())?;
            node.entries.insert(
                query.canonical_name().to_owned(),
                OptionEntry::new_scalar(query, scope.clone(), normalized),
            );
        }

        Ok(build_mutation_outcome(query, scope))
    }

    fn unset_query(
        &mut self,
        scope: OptionScopeSelector,
        query: &OptionQuery,
        only_if_unset: bool,
    ) -> Result<OptionMutationOutcome, RmuxError> {
        let default_entry = self.default_entry_for_scope(query, scope.clone());
        let node = self.node_for_exact_scope_mut(&scope);
        if only_if_unset && node.contains(query.canonical_name(), query.index()) {
            return Err(RmuxError::InvalidSetOption(format!(
                "{} is already set",
                query.canonical_name()
            )));
        }

        if query.is_array() && query.index().is_some() {
            let remove_node = if let Some(entry) = node.entries.get_mut(query.canonical_name()) {
                entry.remove_array_index(query.index().unwrap(), query.separator());
                entry.is_empty()
            } else {
                false
            };
            if remove_node {
                node.entries.remove(query.canonical_name());
            }
        } else if is_global_scope(&scope) {
            if let Some(default_entry) = default_entry {
                node.entries
                    .insert(query.canonical_name().to_owned(), default_entry);
            } else {
                node.entries.remove(query.canonical_name());
            }
        } else {
            node.entries.remove(query.canonical_name());
        }

        Ok(build_mutation_outcome(query, scope))
    }

    fn unset_window_pane_overrides(&mut self, scope: &OptionScopeSelector, name: &str) {
        let OptionScopeSelector::Window(target) = scope else {
            return;
        };
        self.panes.retain(|pane_target, node| {
            let matches_window = pane_target.session_name() == target.session_name()
                && pane_target.window_index() == target.window_index();
            if matches_window {
                node.entries.remove(name);
            }
            !node.is_empty()
        });
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

#[cfg(test)]
#[path = "options/tests.rs"]
mod tests;
