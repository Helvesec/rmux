use std::collections::HashMap;

use rmux_core::{HookBindingView, HookScopeIdentity, LifecycleEvent, PaneId, WindowId};
use rmux_proto::{HookName, PaneTarget, RmuxError, ScopeSelector, Target, WindowTarget};

use crate::pane_terminals::{session_not_found, HandlerState};

impl crate::handler::RequestHandler {
    pub(in crate::handler) async fn refresh_hook_identity_aliases(&self) {
        let mut state = self.state.lock().await;
        prune_dead_hook_identities(&mut state);
    }
}

pub(in crate::handler) fn resolve_hook_scope_identity(
    state: &HandlerState,
    scope: &ScopeSelector,
) -> Result<HookScopeIdentity, RmuxError> {
    match scope {
        ScopeSelector::Global => Ok(HookScopeIdentity::Global),
        ScopeSelector::Session(session_name) => {
            if state.sessions.contains_session(session_name) {
                Ok(HookScopeIdentity::Session(session_name.clone()))
            } else {
                Err(session_not_found(session_name))
            }
        }
        ScopeSelector::Window(target) => {
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
            Ok(HookScopeIdentity::Window {
                session_name: target.session_name().clone(),
                window_id: window.id(),
            })
        }
        ScopeSelector::Pane(target) => {
            let session = state
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{}:{}", target.session_name(), target.window_index()),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(target.pane_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                )
            })?;
            Ok(HookScopeIdentity::Pane {
                session_name: target.session_name().clone(),
                window_id: window.id(),
                pane_id: pane.id(),
            })
        }
    }
}

pub(in crate::handler) fn resolve_hook_scope_identity_for_hook(
    state: &HandlerState,
    scope: &ScopeSelector,
    hook: HookName,
) -> Result<HookScopeIdentity, RmuxError> {
    let natural_scope = match scope {
        ScopeSelector::Global => ScopeSelector::Global,
        ScopeSelector::Session(session_name) => {
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            rmux_core::hook_natural_scope_for_session_target(
                hook,
                session_name.clone(),
                session.active_window_index(),
                session.active_pane_index(),
            )
        }
        ScopeSelector::Window(target) => {
            rmux_core::hook_explicit_scope_for_target(hook, Target::Window(target.clone()))
        }
        ScopeSelector::Pane(target) => {
            rmux_core::hook_explicit_scope_for_target(hook, Target::Pane(target.clone()))
        }
    };
    resolve_hook_scope_identity(state, &natural_scope)
}

pub(in crate::handler) fn lifecycle_hook_scope_identity(
    state: &HandlerState,
    scope: &ScopeSelector,
    event: &LifecycleEvent,
) -> Option<HookScopeIdentity> {
    resolve_hook_scope_identity(state, scope).ok().or_else(|| {
        let session_name = event.session_name()?.clone();
        match scope {
            ScopeSelector::Global => Some(HookScopeIdentity::Global),
            ScopeSelector::Session(_) => Some(HookScopeIdentity::Session(session_name)),
            ScopeSelector::Window(_) => Some(HookScopeIdentity::Window {
                session_name,
                window_id: WindowId::new(event.window_id()?),
            }),
            ScopeSelector::Pane(_) => Some(HookScopeIdentity::Pane {
                session_name,
                window_id: WindowId::new(event.window_id()?),
                pane_id: PaneId::new(event.pane_id()?),
            }),
        }
    })
}

pub(in crate::handler) fn hook_bindings_view(
    state: &HandlerState,
    scope: &ScopeSelector,
    hook: Option<HookName>,
) -> Result<Vec<HookBindingView>, RmuxError> {
    let identity = match hook {
        Some(hook) => resolve_hook_scope_identity_for_hook(state, scope, hook)?,
        None => resolve_hook_scope_identity(state, scope)?,
    };
    Ok(state.hooks.bindings_view_with_identity(&identity, hook))
}

pub(in crate::handler) fn prune_dead_hook_identities(state: &mut HandlerState) {
    let mut window_aliases = HashMap::new();
    let mut pane_aliases = HashMap::new();
    for (session_name, session) in state.sessions.iter() {
        for (window_index, window) in session.windows() {
            window_aliases.insert(
                WindowTarget::with_window(session_name.clone(), *window_index),
                window.id(),
            );
            for pane in window.panes() {
                pane_aliases.insert(
                    PaneTarget::with_window(session_name.clone(), *window_index, pane.index()),
                    (window.id(), pane.id()),
                );
            }
        }
    }
    state
        .hooks
        .replace_identity_aliases(window_aliases, pane_aliases);
}
