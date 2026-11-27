//! Scope resolution for `set-option` from its flags and target.

use rmux_core::ManagedSessionStore;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{RmuxError, SessionName, Target, WindowTarget};

use super::{dummy_pane_scope, is_user_option_name};

pub(super) struct SetOptionScopeContext<'a> {
    pub(super) target: Option<Target>,
    pub(super) sessions: &'a ManagedSessionStore,
}

pub(super) fn resolve_set_option_scope(
    option: &str,
    global: bool,
    server: bool,
    window: bool,
    pane: bool,
    append: bool,
    context: SetOptionScopeContext<'_>,
) -> Result<ResolvedSetOptionScope, RmuxError> {
    let SetOptionScopeContext { target, sessions } = context;
    rmux_core::resolve_option_name(option)?;
    let is_user = is_user_option_name(option);
    let supports_scope = |scope: &OptionScopeSelector| option_name_supports_scope(option, scope);

    if pane && !server && !window {
        match target.clone() {
            Some(Target::Pane(target)) => {
                let scope = OptionScopeSelector::Pane(target);
                if is_user || supports_scope(&scope) {
                    return Ok(scope.into());
                }
            }
            None if is_user || option_supports_pane_scope(option) => {
                return Ok(ResolvedSetOptionScope::NoOp);
            }
            _ => {}
        }
    }

    if global && !is_user && (server || pane || window) {
        let scope = rmux_core::default_global_scope_for_option_name(option)?;
        if supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Err(RmuxError::Server(
            "global scope is not supported for this option".to_owned(),
        ));
    }

    if !global && !is_user && (server || pane || window) {
        let scope = natural_known_set_option_scope(option, target, sessions)?;
        return Ok(scope.into());
    }

    if server {
        let scope = OptionScopeSelector::ServerGlobal;
        if is_user || supports_scope(&scope) {
            return Ok(scope.into());
        }
        return Ok(ResolvedSetOptionScope::NoOp);
    }

    if global && pane {
        return Ok(OptionScopeSelector::WindowGlobal.into());
    }

    if pane {
        let Some(Target::Pane(target)) = target else {
            return Err(RmuxError::Server(
                "set-option -p requires a pane target".to_owned(),
            ));
        };
        let scope = OptionScopeSelector::Pane(target);
        return Ok(scope.into());
    }

    if window {
        if global {
            let scope = OptionScopeSelector::WindowGlobal;
            return Ok(scope.into());
        }

        let Some(target) = target else {
            return Err(RmuxError::Server(
                "set-window-option requires a window target or -g".to_owned(),
            ));
        };
        let scope = match target {
            Target::Session(session_name) => {
                OptionScopeSelector::Window(current_set_option_window(session_name, sessions)?)
            }
            Target::Window(target) => OptionScopeSelector::Window(target),
            Target::Pane(target) => OptionScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            )),
        };
        return Ok(scope.into());
    }

    if global {
        let scope = rmux_core::default_global_scope_for_option_name(option)?;
        if !is_user && !supports_scope(&scope) {
            return Err(RmuxError::Server(
                "global scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(scope.into());
    }

    let Some(target) = target else {
        let default_scope = rmux_core::default_global_scope_for_option_name(option)?;
        if matches!(default_scope, OptionScopeSelector::ServerGlobal) {
            if !is_user && !supports_scope(&default_scope) {
                return Err(RmuxError::Server(
                    "global scope is not supported for this option".to_owned(),
                ));
            }
            return Ok(default_scope.into());
        }
        if !(server && append) {
            return Err(RmuxError::Server(
                "set-option requires a target or one of -g, -s, -w, or -p".to_owned(),
            ));
        }
        if !is_user && !supports_scope(&default_scope) {
            return Err(RmuxError::Server(
                "global scope is not supported for this option".to_owned(),
            ));
        }
        return Ok(default_scope.into());
    };

    let scope = match target.clone() {
        Target::Session(session_name) => OptionScopeSelector::Session(session_name),
        Target::Window(target) => {
            if is_user {
                OptionScopeSelector::Session(target.session_name().clone())
            } else if supports_scope(&OptionScopeSelector::Window(target.clone())) {
                OptionScopeSelector::Window(target)
            } else {
                OptionScopeSelector::Session(target.session_name().clone())
            }
        }
        Target::Pane(target) => {
            if is_user {
                OptionScopeSelector::Session(target.session_name().clone())
            } else if supports_scope(&OptionScopeSelector::Pane(target.clone())) {
                OptionScopeSelector::Pane(target)
            } else if supports_scope(&OptionScopeSelector::Window(WindowTarget::with_window(
                target.session_name().clone(),
                target.window_index(),
            ))) {
                OptionScopeSelector::Window(WindowTarget::with_window(
                    target.session_name().clone(),
                    target.window_index(),
                ))
            } else {
                OptionScopeSelector::Session(target.session_name().clone())
            }
        }
    };

    if !is_user && !supports_scope(&scope) {
        // Fall back to the option's natural table scope (mirroring the
        // explicit-flag path, which trusts natural_known_set_option_scope):
        // e.g. a flagless `set -U -t alpha:0 pane-border-style` resolves at
        // window scope like tmux instead of dead-ending at session scope.
        return Ok(natural_known_set_option_scope(option, Some(target), sessions)?.into());
    }

    Ok(scope.into())
}

fn natural_known_set_option_scope(
    option: &str,
    target: Option<Target>,
    sessions: &ManagedSessionStore,
) -> Result<OptionScopeSelector, RmuxError> {
    match rmux_core::default_global_scope_for_option_name(option)? {
        OptionScopeSelector::ServerGlobal => Ok(OptionScopeSelector::ServerGlobal),
        OptionScopeSelector::SessionGlobal => {
            let Some(target) = target else {
                return Err(RmuxError::Server(
                    "set-option requires a target or one of -g, -s, -w, or -p".to_owned(),
                ));
            };
            Ok(OptionScopeSelector::Session(match target {
                Target::Session(session_name) => session_name,
                Target::Window(target) => target.session_name().clone(),
                Target::Pane(target) => target.session_name().clone(),
            }))
        }
        OptionScopeSelector::WindowGlobal => {
            let Some(target) = target else {
                return Err(RmuxError::Server(
                    "set-window-option requires a window target or -g".to_owned(),
                ));
            };
            Ok(OptionScopeSelector::Window(match target {
                Target::Session(session_name) => current_set_option_window(session_name, sessions)?,
                Target::Window(target) => target,
                Target::Pane(target) => {
                    WindowTarget::with_window(target.session_name().clone(), target.window_index())
                }
            }))
        }
        OptionScopeSelector::Session(session_name) => {
            Ok(OptionScopeSelector::Session(session_name))
        }
        OptionScopeSelector::Window(target) => Ok(OptionScopeSelector::Window(target)),
        OptionScopeSelector::Pane(target) => Ok(OptionScopeSelector::Pane(target)),
    }
}

fn current_set_option_window(
    session_name: SessionName,
    sessions: &ManagedSessionStore,
) -> Result<WindowTarget, RmuxError> {
    let window_index = sessions
        .session(&session_name)
        .ok_or_else(|| RmuxError::SessionNotFound(session_name.to_string()))?
        .active_window_index();
    Ok(WindowTarget::with_window(session_name, window_index))
}

fn option_supports_pane_scope(option: &str) -> bool {
    option_name_supports_scope(option, &dummy_pane_scope())
}

fn option_name_supports_scope(option: &str, scope: &OptionScopeSelector) -> bool {
    rmux_core::resolve_option_name(option)
        .map(|query| query.supports_scope(scope))
        .unwrap_or(false)
}

pub(super) enum ResolvedSetOptionScope {
    Scope(OptionScopeSelector),
    NoOp,
}

impl ResolvedSetOptionScope {
    pub(super) fn into_scope(self) -> Option<OptionScopeSelector> {
        match self {
            Self::Scope(scope) => Some(scope),
            Self::NoOp => None,
        }
    }
}

impl From<OptionScopeSelector> for ResolvedSetOptionScope {
    fn from(scope: OptionScopeSelector) -> Self {
        Self::Scope(scope)
    }
}
