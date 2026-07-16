use rmux_proto::{HookName, ScopeSelector, WindowTarget};

use super::rules::{hook_class, root_for_hook};
use super::types::HookClass;
use super::{HookDispatch, HookStore};

impl HookStore {
    /// Dispatches against a pre-mutation snapshot and commits only the
    /// resulting one-shot consumption to this live store.
    ///
    /// Reusing the same snapshot for an ordered event batch preserves normal
    /// fallback semantics and prevents a global one-shot from being copied to
    /// more than one deferred event.
    pub fn dispatch_deferred_from(
        &mut self,
        snapshot: &mut Self,
        scope: &ScopeSelector,
        hook: HookName,
    ) -> Vec<HookDispatch> {
        let (resolved_scope, one_shot_indices) = snapshot.resolved_dispatch_binding(scope, hook);
        let dispatches = snapshot.dispatch(scope, hook);
        if dispatches.is_empty() {
            return dispatches;
        }
        for index in one_shot_indices {
            let _ = self.unset(resolved_scope.clone(), hook, Some(index));
        }
        dispatches
    }

    pub(super) fn resolved_dispatch_binding(
        &self,
        scope: &ScopeSelector,
        hook: HookName,
    ) -> (ScopeSelector, Vec<u32>) {
        let local_scope = match hook_class(hook) {
            HookClass::Session => match scope {
                ScopeSelector::Session(session_name) => {
                    Some(ScopeSelector::Session(session_name.clone()))
                }
                ScopeSelector::Window(target) => {
                    Some(ScopeSelector::Session(target.session_name().clone()))
                }
                ScopeSelector::Pane(target) => {
                    Some(ScopeSelector::Session(target.session_name().clone()))
                }
                ScopeSelector::Global => None,
            },
            HookClass::Window => match scope {
                ScopeSelector::Window(target) => Some(ScopeSelector::Window(target.clone())),
                ScopeSelector::Pane(target) => Some(ScopeSelector::Window(
                    WindowTarget::with_window(target.session_name().clone(), target.window_index()),
                )),
                ScopeSelector::Global | ScopeSelector::Session(_) => None,
            },
            HookClass::Pane => match scope {
                ScopeSelector::Pane(target) => Some(ScopeSelector::Pane(target.clone())),
                ScopeSelector::Global | ScopeSelector::Session(_) | ScopeSelector::Window(_) => {
                    None
                }
            },
        };

        if let Some(local_scope) = local_scope {
            let indices = self.one_shot_indices_for_exact_scope(&local_scope, hook);
            let has_binding = match &local_scope {
                ScopeSelector::Session(session_name) => self
                    .sessions
                    .get(session_name)
                    .is_some_and(|bindings| bindings.command(hook).is_some()),
                ScopeSelector::Window(target) => self.window_command(target, hook).is_some(),
                ScopeSelector::Pane(target) => self.pane_command(target, hook).is_some(),
                ScopeSelector::Global => false,
            };
            if has_binding {
                return (local_scope, indices);
            }
        }

        let global_scope = ScopeSelector::Global;
        let indices = self.one_shot_indices_for_exact_scope(&global_scope, hook);
        (global_scope, indices)
    }

    fn one_shot_indices_for_exact_scope(&self, scope: &ScopeSelector, hook: HookName) -> Vec<u32> {
        match scope {
            ScopeSelector::Global => self
                .global_bindings(root_for_hook(hook))
                .one_shot_indices(hook),
            ScopeSelector::Session(session_name) => self
                .sessions
                .get(session_name)
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
            ScopeSelector::Window(target) => self
                .windows
                .get(target)
                .or_else(|| {
                    self.window_aliases
                        .get(target)
                        .and_then(|window_id| self.windows_by_id.get(window_id))
                })
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
            ScopeSelector::Pane(target) => self
                .panes
                .get(target)
                .or_else(|| {
                    self.pane_aliases
                        .get(target)
                        .and_then(|(_, pane_id)| self.panes_by_id.get(pane_id))
                })
                .map_or_else(Vec::new, |bindings| bindings.one_shot_indices(hook)),
        }
    }
}
