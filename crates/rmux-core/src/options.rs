use std::collections::HashMap;

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    OptionName, PaneTarget, RmuxError, ScopeSelector, SessionName, SetOptionMode, WindowTarget,
};

mod access;
mod lifecycle;
mod mutation;
mod registry;
mod render;
mod scope;
mod show;
mod storage;

use mutation::{
    apply_array_mutation, build_mutation_outcome, is_global_scope, legacy_scope_for_option,
    normalize_scalar_value,
};
pub use mutation::{validate_option_mutation, validate_option_name_mutation};
pub use registry::{
    option_affects_alerts, option_affects_rendering, option_name_by_name, resolve_option_name,
    resolve_option_name_typed, OptionLookupError, OptionQuery,
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
    /// The exact scope mutated by this outcome.
    pub scope: OptionScopeSelector,
    /// The known wire option, when the option is part of the closed V1 registry.
    pub known_option: Option<OptionName>,
    /// Exact explicit value before the mutation at the mutated scope.
    pub old_explicit: Option<String>,
    /// Exact explicit value after the mutation at the mutated scope.
    pub new_explicit: Option<String>,
    /// Whether the exact explicit value changed.
    ///
    /// Idempotent sets and no-op unsets return `false`; event producers should
    /// use this bit to avoid echoing mutations that did not change pane state.
    pub changed: bool,
    /// Side effects the server may react to.
    pub notifications: Vec<OptionNotification>,
    /// Additional exact-scope mutations caused by this operation.
    pub related: Vec<OptionMutationOutcome>,
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
        // tmux 3.7b -U (oracle-probed 2026-07-09): acts like -u at the
        // resolved scope, and additionally clears the window's pane
        // overrides only when that scope is a window. Plain `set -U` unsets
        // the session copy alone; `set -pU` unsets the pane copy alone.
        let query = validate_option_name_mutation(name, &scope, mode, value.as_deref(), unset)?;
        let unset_pane_scope = (unset_pane_overrides
            && matches!(scope, OptionScopeSelector::Window(_)))
        .then(|| scope.clone());

        let mut outcome = if unset {
            self.unset_query(scope, &query, only_if_unset)
        } else {
            self.set_query(scope, &query, value.as_deref(), mode, only_if_unset)
        }?;
        let related = if let Some(scope) = unset_pane_scope {
            self.unset_window_pane_overrides(&scope, &query)
        } else {
            Vec::new()
        };
        outcome.related.extend(related);
        Ok(outcome)
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
            return Ok(build_mutation_outcome(
                query,
                scope,
                explicit_before.clone(),
                explicit_before,
            ));
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

        let explicit_after = self.explicit_value_for_scope(&scope, query);
        Ok(build_mutation_outcome(
            query,
            scope,
            explicit_before,
            explicit_after,
        ))
    }

    fn unset_query(
        &mut self,
        scope: OptionScopeSelector,
        query: &OptionQuery,
        only_if_unset: bool,
    ) -> Result<OptionMutationOutcome, RmuxError> {
        let explicit_before = self.explicit_value_for_scope(&scope, query);
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

        let explicit_after = self.explicit_value_for_scope(&scope, query);
        Ok(build_mutation_outcome(
            query,
            scope,
            explicit_before,
            explicit_after,
        ))
    }

    fn unset_window_pane_overrides(
        &mut self,
        scope: &OptionScopeSelector,
        query: &OptionQuery,
    ) -> Vec<OptionMutationOutcome> {
        let OptionScopeSelector::Window(target) = scope else {
            return Vec::new();
        };
        let name = query.canonical_name();
        let mut outcomes = Vec::new();
        self.panes.retain(|pane_target, node| {
            let matches_window = pane_target.session_name() == target.session_name()
                && pane_target.window_index() == target.window_index();
            if matches_window {
                if let Some(removed_entry) = node.entries.remove(name) {
                    outcomes.push(build_mutation_outcome(
                        query,
                        OptionScopeSelector::Pane(pane_target.clone()),
                        Some(removed_entry.rendered().to_owned()),
                        None,
                    ));
                }
            }
            !node.is_empty()
        });
        outcomes
    }
}

#[cfg(test)]
#[path = "options/tests.rs"]
mod tests;
