//! Option scope conversion and the windows an option scope reaches.

use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, WindowTarget};

pub(super) fn legacy_scope_to_refresh_scope(
    scope: &rmux_proto::ScopeSelector,
) -> OptionScopeSelector {
    match scope {
        rmux_proto::ScopeSelector::Global => OptionScopeSelector::SessionGlobal,
        rmux_proto::ScopeSelector::Session(session_name) => {
            OptionScopeSelector::Session(session_name.clone())
        }
        rmux_proto::ScopeSelector::Window(target) => OptionScopeSelector::Window(target.clone()),
        rmux_proto::ScopeSelector::Pane(target) => OptionScopeSelector::Pane(target.clone()),
    }
}

pub(super) fn option_scope_to_legacy_scope(
    scope: &OptionScopeSelector,
) -> Option<rmux_proto::ScopeSelector> {
    match scope {
        OptionScopeSelector::ServerGlobal => Some(rmux_proto::ScopeSelector::Global),
        OptionScopeSelector::SessionGlobal => Some(rmux_proto::ScopeSelector::Global),
        OptionScopeSelector::WindowGlobal => None,
        OptionScopeSelector::Session(session_name) => {
            Some(rmux_proto::ScopeSelector::Session(session_name.clone()))
        }
        OptionScopeSelector::Window(target) => {
            Some(rmux_proto::ScopeSelector::Window(target.clone()))
        }
        OptionScopeSelector::Pane(target) => Some(rmux_proto::ScopeSelector::Pane(target.clone())),
    }
}

pub(super) fn automatic_rename_targets_for_scope(
    state: &mut crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<WindowTarget> {
    let targets = window_targets_for_option_scope(state, scope);
    for target in &targets {
        if state.options.resolve_for_window(
            target.session_name(),
            target.window_index(),
            OptionName::AutomaticRename,
        ) != Some("on")
        {
            continue;
        }
        let _ = state.sessions.enable_automatic_window_name(target);
    }
    targets
}

fn window_targets_for_option_scope(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
) -> Vec<WindowTarget> {
    match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => state
            .sessions
            .iter()
            .map(|session| (session.name(), session))
            .flat_map(|(session_name, session)| {
                session
                    .windows()
                    .map(|(index, _)| index)
                    .map(|window_index| {
                        WindowTarget::with_window(session_name.clone(), window_index)
                    })
                    .collect::<Vec<_>>()
            })
            .collect(),
        OptionScopeSelector::Session(session_name) => state
            .sessions
            .session(session_name)
            .map(|session| {
                session
                    .windows()
                    .map(|(index, _)| index)
                    .map(|window_index| {
                        WindowTarget::with_window(session_name.clone(), window_index)
                    })
                    .collect()
            })
            .unwrap_or_default(),
        OptionScopeSelector::Window(target) => vec![target.clone()],
        OptionScopeSelector::Pane(target) => vec![WindowTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
        )],
    }
}
