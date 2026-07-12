use std::collections::{HashMap, HashSet};

#[cfg(test)]
use rmux_proto::SessionName;
use rmux_proto::{
    HookLifecycle, HookName, PaneId, PaneTarget, ScopeSelector, WindowId, WindowTarget,
};

use super::rules::{hook_class, root_for_hook};
use super::types::HookClass;
use super::{
    HookBindingView, HookBindings, HookDispatch, HookScopeIdentity, HookSetOptions, HookStore,
};

impl HookStore {
    /// Registers or mutates a hook at a stable, already-resolved scope.
    pub fn set_with_identity(
        &mut self,
        scope: HookScopeIdentity,
        hook: HookName,
        command: String,
        lifecycle: HookLifecycle,
        options: HookSetOptions,
    ) -> u32 {
        self.identity_bindings_mut(&scope, hook)
            .set(hook, command, lifecycle, options)
    }

    /// Removes a hook, or one indexed element, from a stable scope.
    pub fn unset_with_identity(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
        index: Option<u32>,
    ) {
        match scope {
            HookScopeIdentity::Global => {
                self.global_bindings_mut(root_for_hook(hook))
                    .unset(hook, index);
            }
            HookScopeIdentity::Session(session_name) => {
                let remove_scope = self.sessions.get_mut(session_name).is_some_and(|bindings| {
                    bindings.unset(hook, index);
                    bindings.is_empty()
                });
                if remove_scope {
                    self.sessions.remove(session_name);
                }
            }
            HookScopeIdentity::Window { window_id, .. } => {
                let remove_scope = self
                    .windows_by_id
                    .get_mut(window_id)
                    .is_some_and(|bindings| {
                        bindings.unset(hook, index);
                        bindings.is_empty()
                    });
                if remove_scope {
                    self.windows_by_id.remove(window_id);
                }
            }
            HookScopeIdentity::Pane { pane_id, .. } => {
                let remove_scope = self.panes_by_id.get_mut(pane_id).is_some_and(|bindings| {
                    bindings.unset(hook, index);
                    bindings.is_empty()
                });
                if remove_scope {
                    self.panes_by_id.remove(pane_id);
                }
            }
        }
    }

    /// Returns explicit bindings stored at a stable scope.
    #[must_use]
    pub fn bindings_view_with_identity(
        &self,
        scope: &HookScopeIdentity,
        hook: Option<HookName>,
    ) -> Vec<HookBindingView> {
        match scope {
            HookScopeIdentity::Global => Vec::new(),
            HookScopeIdentity::Session(session_name) => self
                .sessions
                .get(session_name)
                .map_or_else(Vec::new, |bindings| bindings.views(hook)),
            HookScopeIdentity::Window { window_id, .. } => self
                .windows_by_id
                .get(window_id)
                .map_or_else(Vec::new, |bindings| bindings.views(hook)),
            HookScopeIdentity::Pane { pane_id, .. } => self
                .panes_by_id
                .get(pane_id)
                .map_or_else(Vec::new, |bindings| bindings.views(hook)),
        }
    }

    /// Resolves and consumes a hook using stable window and pane identities.
    #[must_use]
    pub fn dispatch_with_identity(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        match hook_class(hook) {
            HookClass::Session => self.dispatch_identity_session(scope, hook),
            HookClass::Window => self.dispatch_identity_window(scope, hook),
            HookClass::Pane => self.dispatch_identity_pane(scope, hook),
        }
    }

    /// Dispatches through stable identity storage while retaining compatibility
    /// with callers that populated the legacy slot-keyed API directly.
    #[must_use]
    pub fn dispatch_with_identity_or_scope(
        &mut self,
        identity: &HookScopeIdentity,
        scope: &ScopeSelector,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        let (resolved_identity, _) = self.resolved_identity_binding(identity, hook);
        if !matches!(resolved_identity, HookScopeIdentity::Global) {
            return self.dispatch_with_identity(identity, hook);
        }
        let (resolved_scope, _) = self.resolved_dispatch_binding(scope, hook);
        if !matches!(resolved_scope, ScopeSelector::Global) {
            return self.dispatch(scope, hook);
        }
        self.dispatch_with_identity(identity, hook)
    }

    /// Dispatches from a pre-mutation snapshot and commits one-shot
    /// consumption to the live identity-keyed store.
    pub fn dispatch_deferred_with_identity(
        &mut self,
        snapshot: &mut Self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        let (resolved_scope, one_shots) = snapshot.resolved_identity_binding(scope, hook);
        let dispatches = snapshot.dispatch_with_identity(scope, hook);
        if dispatches.is_empty() {
            return dispatches;
        }
        for index in one_shots {
            self.unset_with_identity(&resolved_scope, hook, Some(index));
        }
        dispatches
    }

    /// Deferred counterpart of [`Self::dispatch_with_identity_or_scope`].
    pub fn dispatch_deferred_with_identity_or_scope(
        &mut self,
        snapshot: &mut Self,
        identity: &HookScopeIdentity,
        scope: &ScopeSelector,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        let (resolved_identity, _) = snapshot.resolved_identity_binding(identity, hook);
        if !matches!(resolved_identity, HookScopeIdentity::Global) {
            return self.dispatch_deferred_with_identity(snapshot, identity, hook);
        }
        let (resolved_scope, _) = snapshot.resolved_dispatch_binding(scope, hook);
        if !matches!(resolved_scope, ScopeSelector::Global) {
            return self.dispatch_deferred_from(snapshot, scope, hook);
        }
        self.dispatch_deferred_with_identity(snapshot, identity, hook)
    }

    /// Drops identity-keyed bindings whose owning objects no longer exist.
    pub fn retain_identities(
        &mut self,
        window_ids: &HashSet<WindowId>,
        pane_ids: &HashSet<PaneId>,
    ) {
        self.windows_by_id
            .retain(|window_id, _| window_ids.contains(window_id));
        self.panes_by_id
            .retain(|pane_id, _| pane_ids.contains(pane_id));
    }

    /// Atomically replaces slot aliases used by the compatibility API and
    /// purges bindings for identities that are no longer live.
    pub fn replace_identity_aliases(
        &mut self,
        window_aliases: HashMap<WindowTarget, WindowId>,
        pane_aliases: HashMap<PaneTarget, (WindowId, PaneId)>,
    ) {
        let legacy_windows = std::mem::take(&mut self.windows);
        for (target, bindings) in legacy_windows {
            if let Some(window_id) = window_aliases.get(&target) {
                self.windows_by_id.entry(*window_id).or_insert(bindings);
            } else {
                self.windows.insert(target, bindings);
            }
        }
        let legacy_panes = std::mem::take(&mut self.panes);
        for (target, bindings) in legacy_panes {
            if let Some((_, pane_id)) = pane_aliases.get(&target) {
                self.panes_by_id.entry(*pane_id).or_insert(bindings);
            } else {
                self.panes.insert(target, bindings);
            }
        }

        let window_ids = window_aliases.values().copied().collect::<HashSet<_>>();
        let pane_ids = pane_aliases
            .values()
            .map(|(_, pane_id)| *pane_id)
            .collect::<HashSet<_>>();
        self.retain_identities(&window_ids, &pane_ids);
        self.window_aliases = window_aliases;
        self.pane_aliases = pane_aliases;
    }

    /// Returns the first exact identity-keyed window command.
    #[must_use]
    pub fn window_command_by_id(&self, window_id: WindowId, hook: HookName) -> Option<&str> {
        self.windows_by_id
            .get(&window_id)
            .and_then(|bindings| bindings.command(hook))
    }

    /// Returns the first exact identity-keyed pane command.
    #[must_use]
    pub fn pane_command_by_id(&self, pane_id: PaneId, hook: HookName) -> Option<&str> {
        self.panes_by_id
            .get(&pane_id)
            .and_then(|bindings| bindings.command(hook))
    }

    fn identity_bindings_mut(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> &mut HookBindings {
        match scope {
            HookScopeIdentity::Global => self.global_bindings_mut(root_for_hook(hook)),
            HookScopeIdentity::Session(session_name) => {
                self.sessions.entry(session_name.clone()).or_default()
            }
            HookScopeIdentity::Window { window_id, .. } => {
                self.windows_by_id.entry(*window_id).or_default()
            }
            HookScopeIdentity::Pane { pane_id, .. } => {
                self.panes_by_id.entry(*pane_id).or_default()
            }
        }
    }

    fn dispatch_identity_session(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        if let Some(session_name) = scope.session_name() {
            let (dispatches, remove_scope) =
                self.sessions
                    .get_mut(session_name)
                    .map_or((Vec::new(), false), |bindings| {
                        let dispatches = bindings.dispatch(hook);
                        (dispatches, bindings.is_empty())
                    });
            if remove_scope {
                self.sessions.remove(session_name);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }
        self.session_global.dispatch(hook)
    }

    fn dispatch_identity_window(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        let window_id = match scope {
            HookScopeIdentity::Window { window_id, .. }
            | HookScopeIdentity::Pane { window_id, .. } => Some(*window_id),
            HookScopeIdentity::Global | HookScopeIdentity::Session(_) => None,
        };
        if let Some(window_id) = window_id {
            let (dispatches, remove_scope) =
                self.windows_by_id
                    .get_mut(&window_id)
                    .map_or((Vec::new(), false), |bindings| {
                        let dispatches = bindings.dispatch(hook);
                        (dispatches, bindings.is_empty())
                    });
            if remove_scope {
                self.windows_by_id.remove(&window_id);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }
        self.window_global.dispatch(hook)
    }

    fn dispatch_identity_pane(
        &mut self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        if let HookScopeIdentity::Pane { pane_id, .. } = scope {
            let (dispatches, remove_scope) =
                self.panes_by_id
                    .get_mut(pane_id)
                    .map_or((Vec::new(), false), |bindings| {
                        let dispatches = bindings.dispatch(hook);
                        (dispatches, bindings.is_empty())
                    });
            if remove_scope {
                self.panes_by_id.remove(pane_id);
            }
            if !dispatches.is_empty() {
                return dispatches;
            }
        }
        self.dispatch_identity_window(scope, hook)
    }

    fn resolved_identity_binding(
        &self,
        scope: &HookScopeIdentity,
        hook: HookName,
    ) -> (HookScopeIdentity, Vec<u32>) {
        if hook_class(hook) == HookClass::Pane {
            if let HookScopeIdentity::Pane {
                session_name,
                window_id,
                ..
            } = scope
            {
                if self.identity_scope_has_binding(scope, hook) {
                    return (
                        scope.clone(),
                        self.one_shot_indices_for_identity(scope, hook),
                    );
                }
                let window_scope = HookScopeIdentity::Window {
                    session_name: session_name.clone(),
                    window_id: *window_id,
                };
                if self.identity_scope_has_binding(&window_scope, hook) {
                    let indices = self.one_shot_indices_for_identity(&window_scope, hook);
                    return (window_scope, indices);
                }
            }
            let global = HookScopeIdentity::Global;
            let indices = self.one_shot_indices_for_identity(&global, hook);
            return (global, indices);
        }

        let local_scope = match hook_class(hook) {
            HookClass::Session => scope
                .session_name()
                .cloned()
                .map(HookScopeIdentity::Session),
            HookClass::Window => match scope {
                HookScopeIdentity::Window {
                    session_name,
                    window_id,
                }
                | HookScopeIdentity::Pane {
                    session_name,
                    window_id,
                    ..
                } => Some(HookScopeIdentity::Window {
                    session_name: session_name.clone(),
                    window_id: *window_id,
                }),
                HookScopeIdentity::Global | HookScopeIdentity::Session(_) => None,
            },
            HookClass::Pane => unreachable!("pane hooks handled above"),
        };

        if let Some(local_scope) = local_scope {
            let indices = self.one_shot_indices_for_identity(&local_scope, hook);
            if self.identity_scope_has_binding(&local_scope, hook) {
                return (local_scope, indices);
            }
        }

        let global = HookScopeIdentity::Global;
        let indices = self.one_shot_indices_for_identity(&global, hook);
        (global, indices)
    }

    fn identity_scope_has_binding(&self, scope: &HookScopeIdentity, hook: HookName) -> bool {
        match scope {
            HookScopeIdentity::Global => self.global_bindings(root_for_hook(hook)),
            HookScopeIdentity::Session(session_name) => match self.sessions.get(session_name) {
                Some(bindings) => bindings,
                None => return false,
            },
            HookScopeIdentity::Window { window_id, .. } => {
                match self.windows_by_id.get(window_id) {
                    Some(bindings) => bindings,
                    None => return false,
                }
            }
            HookScopeIdentity::Pane { pane_id, .. } => match self.panes_by_id.get(pane_id) {
                Some(bindings) => bindings,
                None => return false,
            },
        }
        .command(hook)
        .is_some()
    }

    fn one_shot_indices_for_identity(&self, scope: &HookScopeIdentity, hook: HookName) -> Vec<u32> {
        match scope {
            HookScopeIdentity::Global => self
                .global_bindings(root_for_hook(hook))
                .one_shot_indices(hook),
            HookScopeIdentity::Session(session_name) => self
                .sessions
                .get(session_name)
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
            HookScopeIdentity::Window { window_id, .. } => self
                .windows_by_id
                .get(window_id)
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
            HookScopeIdentity::Pane { pane_id, .. } => self
                .panes_by_id
                .get(pane_id)
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_name(value: &str) -> SessionName {
        SessionName::new(value).expect("valid session name")
    }

    fn pane_scope(session: &str) -> HookScopeIdentity {
        HookScopeIdentity::Pane {
            session_name: session_name(session),
            window_id: WindowId::new(7),
            pane_id: PaneId::new(11),
        }
    }

    #[test]
    fn aliases_share_one_pane_binding_and_one_shot_consumption() {
        let mut store = HookStore::new();
        store.set_with_identity(
            pane_scope("owner"),
            HookName::PaneExited,
            "set-option @once yes".to_owned(),
            HookLifecycle::OneShot,
            HookSetOptions::default(),
        );

        let first = store.dispatch_with_identity(&pane_scope("peer"), HookName::PaneExited);
        let second = store.dispatch_with_identity(&pane_scope("owner"), HookName::PaneExited);

        assert_eq!(first.len(), 1);
        assert!(second.is_empty());
    }

    #[test]
    fn window_aliases_share_bindings_but_session_hooks_do_not() {
        let owner = HookScopeIdentity::Window {
            session_name: session_name("owner"),
            window_id: WindowId::new(7),
        };
        let peer = HookScopeIdentity::Window {
            session_name: session_name("peer"),
            window_id: WindowId::new(7),
        };
        let mut store = HookStore::new();
        store.set_with_identity(
            owner,
            HookName::WindowLayoutChanged,
            "set-option @window yes".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions::default(),
        );
        store.set_with_identity(
            HookScopeIdentity::Session(session_name("owner")),
            HookName::ClientAttached,
            "set-option @session yes".to_owned(),
            HookLifecycle::Persistent,
            HookSetOptions::default(),
        );

        assert_eq!(
            store
                .dispatch_with_identity(&peer, HookName::WindowLayoutChanged)
                .len(),
            1
        );
        assert!(store
            .dispatch_with_identity(&peer, HookName::ClientAttached)
            .is_empty());
    }

    #[test]
    fn retaining_live_ids_purges_destroyed_objects_only() {
        let mut store = HookStore::new();
        for pane_id in [PaneId::new(1), PaneId::new(2)] {
            store.set_with_identity(
                HookScopeIdentity::Pane {
                    session_name: session_name("owner"),
                    window_id: WindowId::new(pane_id.as_u32()),
                    pane_id,
                },
                HookName::PaneExited,
                format!("set-option @pane{} yes", pane_id.as_u32()),
                HookLifecycle::Persistent,
                HookSetOptions::default(),
            );
        }

        store.retain_identities(
            &HashSet::from([WindowId::new(2)]),
            &HashSet::from([PaneId::new(2)]),
        );

        assert_eq!(
            store.pane_command_by_id(PaneId::new(1), HookName::PaneExited),
            None
        );
        assert!(store
            .pane_command_by_id(PaneId::new(2), HookName::PaneExited)
            .is_some());
    }
}
