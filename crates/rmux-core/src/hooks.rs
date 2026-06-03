use std::collections::{BTreeMap, HashMap};

use rmux_proto::{
    HookLifecycle, HookName, PaneTarget, RmuxError, ScopeSelector, SessionName, WindowTarget,
};

#[path = "hooks/bindings.rs"]
mod bindings;
#[path = "hooks/rules.rs"]
mod rules;
#[path = "hooks/types.rs"]
mod types;

use bindings::HookBindings;
use rules::{hook_class, hook_inventory, hook_is_visible_in_show_hooks, root_for_hook};
pub use rules::{validate_hook_registration, validate_hook_scope};
use types::HookClass;
pub use types::{HookBindingView, HookDispatch, HookGlobalRoot, HookSetOptions};

/// In-memory storage for tmux-style hook arrays.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HookStore {
    session_global: HookBindings,
    window_global: HookBindings,
    sessions: HashMap<SessionName, HookBindings>,
    windows: HashMap<WindowTarget, HookBindings>,
    panes: HashMap<PaneTarget, HookBindings>,
}

impl HookStore {
    /// Creates an empty hook store with no registered hooks.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether no explicit hooks are present at any scope.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.session_global.is_empty()
            && self.window_global.is_empty()
            && self.sessions.values().all(HookBindings::is_empty)
            && self.windows.values().all(HookBindings::is_empty)
            && self.panes.values().all(HookBindings::is_empty)
    }

    /// Registers or replaces a hook using tmux's default index-zero semantics.
    pub fn set(
        &mut self,
        scope: ScopeSelector,
        hook: HookName,
        command: String,
        lifecycle: HookLifecycle,
    ) -> Result<u32, RmuxError> {
        self.set_with_options(scope, hook, command, lifecycle, HookSetOptions::default())
    }

    /// Registers or mutates a hook using indexed tmux array semantics.
    pub fn set_with_options(
        &mut self,
        scope: ScopeSelector,
        hook: HookName,
        command: String,
        lifecycle: HookLifecycle,
        options: HookSetOptions,
    ) -> Result<u32, RmuxError> {
        validate_hook_scope(hook, &scope)?;
        let bindings = self.bindings_for_scope_mut(hook, &scope);
        Ok(bindings.set(hook, command, lifecycle, options))
    }

    /// Removes a hook or a single indexed hook element.
    pub fn unset(
        &mut self,
        scope: ScopeSelector,
        hook: HookName,
        index: Option<u32>,
    ) -> Result<(), RmuxError> {
        validate_hook_scope(hook, &scope)?;
        match scope {
            ScopeSelector::Global => {
                self.global_bindings_mut(root_for_hook(hook))
                    .unset(hook, index);
            }
            ScopeSelector::Session(session_name) => {
                let remove_scope = if let Some(bindings) = self.sessions.get_mut(&session_name) {
                    bindings.unset(hook, index);
                    bindings.is_empty()
                } else {
                    false
                };
                if remove_scope {
                    self.sessions.remove(&session_name);
                }
            }
            ScopeSelector::Window(target) => {
                let remove_scope = if let Some(bindings) = self.windows.get_mut(&target) {
                    bindings.unset(hook, index);
                    bindings.is_empty()
                } else {
                    false
                };
                if remove_scope {
                    self.windows.remove(&target);
                }
            }
            ScopeSelector::Pane(target) => {
                let remove_scope = if let Some(bindings) = self.panes.get_mut(&target) {
                    bindings.unset(hook, index);
                    bindings.is_empty()
                } else {
                    false
                };
                if remove_scope {
                    self.panes.remove(&target);
                }
            }
        }
        Ok(())
    }

    /// Returns the first explicit global command for the given hook, when present.
    #[must_use]
    pub fn global_command(&self, hook: HookName) -> Option<&str> {
        self.global_bindings(root_for_hook(hook)).command(hook)
    }

    /// Returns the exact global command at the given array index, when present.
    #[must_use]
    pub fn global_command_at(&self, hook: HookName, index: u32) -> Option<&str> {
        self.global_bindings(root_for_hook(hook))
            .command_at(hook, index)
    }

    /// Returns the first explicit global lifecycle for the given hook, when present.
    #[must_use]
    pub fn global_lifecycle(&self, hook: HookName) -> Option<HookLifecycle> {
        self.global_bindings(root_for_hook(hook)).lifecycle(hook)
    }

    /// Returns the exact global lifecycle at the given array index, when present.
    #[must_use]
    pub fn global_lifecycle_at(&self, hook: HookName, index: u32) -> Option<HookLifecycle> {
        self.global_bindings(root_for_hook(hook))
            .lifecycle_at(hook, index)
    }

    /// Returns the first exact session-local command for the given hook, when present.
    #[must_use]
    pub fn session_command(&self, session_name: &SessionName, hook: HookName) -> Option<&str> {
        self.sessions
            .get(session_name)
            .and_then(|bindings| bindings.command(hook))
    }

    /// Returns the exact session-local command at the given array index, when present.
    #[must_use]
    pub fn session_command_at(
        &self,
        session_name: &SessionName,
        hook: HookName,
        index: u32,
    ) -> Option<&str> {
        self.sessions
            .get(session_name)
            .and_then(|bindings| bindings.command_at(hook, index))
    }

    /// Returns the first exact session-local lifecycle for the given hook, when present.
    #[must_use]
    pub fn session_lifecycle(
        &self,
        session_name: &SessionName,
        hook: HookName,
    ) -> Option<HookLifecycle> {
        self.sessions
            .get(session_name)
            .and_then(|bindings| bindings.lifecycle(hook))
    }

    /// Returns the exact session-local lifecycle at the given array index, when present.
    #[must_use]
    pub fn session_lifecycle_at(
        &self,
        session_name: &SessionName,
        hook: HookName,
        index: u32,
    ) -> Option<HookLifecycle> {
        self.sessions
            .get(session_name)
            .and_then(|bindings| bindings.lifecycle_at(hook, index))
    }

    /// Returns the first exact window-local command for the given hook, when present.
    #[must_use]
    pub fn window_command(&self, target: &WindowTarget, hook: HookName) -> Option<&str> {
        self.windows
            .get(target)
            .and_then(|bindings| bindings.command(hook))
    }

    /// Returns the first exact pane-local command for the given hook, when present.
    #[must_use]
    pub fn pane_command(&self, target: &PaneTarget, hook: HookName) -> Option<&str> {
        self.panes
            .get(target)
            .and_then(|bindings| bindings.command(hook))
    }

    /// Returns the explicit hook bindings for the requested global root.
    #[must_use]
    pub fn global_bindings_view(
        &self,
        root: HookGlobalRoot,
        hook: Option<HookName>,
    ) -> Vec<HookBindingView> {
        self.global_bindings(root).views(hook)
    }

    /// Returns the explicit session-local hook bindings.
    #[must_use]
    pub fn session_bindings_view(
        &self,
        session_name: &SessionName,
        hook: Option<HookName>,
    ) -> Vec<HookBindingView> {
        self.sessions
            .get(session_name)
            .map_or_else(Vec::new, |bindings| bindings.views(hook))
    }

    /// Returns the explicit window-local hook bindings.
    #[must_use]
    pub fn window_bindings_view(
        &self,
        target: &WindowTarget,
        hook: Option<HookName>,
    ) -> Vec<HookBindingView> {
        self.windows
            .get(target)
            .map_or_else(Vec::new, |bindings| bindings.views(hook))
    }

    /// Returns the explicit pane-local hook bindings.
    #[must_use]
    pub fn pane_bindings_view(
        &self,
        target: &PaneTarget,
        hook: Option<HookName>,
    ) -> Vec<HookBindingView> {
        self.panes
            .get(target)
            .map_or_else(Vec::new, |bindings| bindings.views(hook))
    }

    /// Returns the tmux-compatible hook inventory visible at the requested global root.
    #[must_use]
    pub fn shipped_global_hooks(root: HookGlobalRoot, hook: Option<HookName>) -> Vec<HookName> {
        hook_inventory()
            .into_iter()
            .filter(|candidate| hook.map(|expected| *candidate == expected).unwrap_or(true))
            .filter(|candidate| {
                hook_is_visible_in_show_hooks(*candidate) && root_for_hook(*candidate) == root
            })
            .collect()
    }

    /// Resolves a hook for the provided event scope and returns the matching command batch.
    #[must_use]
    pub fn dispatch(&mut self, scope: &ScopeSelector, hook: HookName) -> Vec<HookDispatch> {
        match hook_class(hook) {
            HookClass::Session => self.dispatch_session(scope, hook),
            HookClass::Window => self.dispatch_window(scope, hook),
            HookClass::Pane => self.dispatch_pane(scope, hook),
        }
    }

    /// Removes all hooks owned by the given session.
    pub fn remove_session(&mut self, session_name: &SessionName) -> bool {
        let mut removed = self.sessions.remove(session_name).is_some();
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
        self.panes.retain(|pane_target, _| {
            let keep = pane_target.session_name() != target.session_name()
                || pane_target.window_index() != target.window_index();
            removed |= !keep;
            keep
        });
        removed
    }

    /// Removes all hooks owned by the given pane.
    pub fn remove_pane(&mut self, target: &PaneTarget) -> bool {
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

    fn bindings_for_scope_mut(
        &mut self,
        hook: HookName,
        scope: &ScopeSelector,
    ) -> &mut HookBindings {
        match scope {
            ScopeSelector::Global => self.global_bindings_mut(root_for_hook(hook)),
            ScopeSelector::Session(session_name) => {
                self.sessions.entry(session_name.clone()).or_default()
            }
            ScopeSelector::Window(target) => self.windows.entry(target.clone()).or_default(),
            ScopeSelector::Pane(target) => self.panes.entry(target.clone()).or_default(),
        }
    }

    fn global_bindings(&self, root: HookGlobalRoot) -> &HookBindings {
        match root {
            HookGlobalRoot::Session => &self.session_global,
            HookGlobalRoot::Window => &self.window_global,
        }
    }

    fn global_bindings_mut(&mut self, root: HookGlobalRoot) -> &mut HookBindings {
        match root {
            HookGlobalRoot::Session => &mut self.session_global,
            HookGlobalRoot::Window => &mut self.window_global,
        }
    }

    fn dispatch_session(&mut self, scope: &ScopeSelector, hook: HookName) -> Vec<HookDispatch> {
        let session_name = match scope {
            ScopeSelector::Session(session_name) => Some(session_name.clone()),
            ScopeSelector::Window(target) => Some(target.session_name().clone()),
            ScopeSelector::Pane(target) => Some(target.session_name().clone()),
            ScopeSelector::Global => None,
        };

        if let Some(session_name) = session_name {
            let (dispatches, remove_scope) =
                if let Some(bindings) = self.sessions.get_mut(&session_name) {
                    let dispatches = bindings.dispatch(hook);
                    let should_remove = bindings.is_empty();
                    (dispatches, should_remove)
                } else {
                    (Vec::new(), false)
                };
            if remove_scope {
                self.sessions.remove(&session_name);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }

        self.session_global.dispatch(hook)
    }

    fn dispatch_window(&mut self, scope: &ScopeSelector, hook: HookName) -> Vec<HookDispatch> {
        let target = match scope {
            ScopeSelector::Window(target) => Some(target.clone()),
            ScopeSelector::Pane(target) => Some(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
            ScopeSelector::Global | ScopeSelector::Session(_) => None,
        };

        if let Some(target) = target {
            let (dispatches, remove_scope) = if let Some(bindings) = self.windows.get_mut(&target) {
                let dispatches = bindings.dispatch(hook);
                let should_remove = bindings.is_empty();
                (dispatches, should_remove)
            } else {
                (Vec::new(), false)
            };
            if remove_scope {
                self.windows.remove(&target);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }

        self.window_global.dispatch(hook)
    }

    fn dispatch_pane(&mut self, scope: &ScopeSelector, hook: HookName) -> Vec<HookDispatch> {
        if let ScopeSelector::Pane(target) = scope {
            let target = target.clone();
            let (dispatches, remove_scope) = if let Some(bindings) = self.panes.get_mut(&target) {
                let dispatches = bindings.dispatch(hook);
                let should_remove = bindings.is_empty();
                (dispatches, should_remove)
            } else {
                (Vec::new(), false)
            };
            if remove_scope {
                self.panes.remove(&target);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }

        self.dispatch_window(scope, hook)
    }
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
#[path = "hooks/tests.rs"]
mod tests;
