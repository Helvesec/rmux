use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{OptionName, RmuxError, ScopeSelector, SessionName};

use crate::pane_terminals::HandlerState;

pub(super) fn resize_terminals_for_option_change(
    state: &mut HandlerState,
    option: OptionName,
    scope: &ScopeSelector,
) -> Result<Vec<SessionName>, RmuxError> {
    if !option_affects_pane_terminal_geometry(option) {
        return Ok(Vec::new());
    }

    let (session_names, linked_refreshes) = match scope {
        ScopeSelector::Global => (all_session_names(state), Vec::new()),
        ScopeSelector::Session(session_name) => (vec![session_name.clone()], Vec::new()),
        ScopeSelector::Window(target) => {
            linked_geometry_sessions(state, target.session_name(), target.window_index())
        }
        ScopeSelector::Pane(target) => {
            linked_geometry_sessions(state, target.session_name(), target.window_index())
        }
    };
    resize_terminals_for_sessions(state, session_names)?;
    Ok(linked_refreshes)
}

pub(super) fn resize_terminals_for_named_option_change(
    state: &mut HandlerState,
    option: OptionName,
    scope: &OptionScopeSelector,
) -> Result<Vec<SessionName>, RmuxError> {
    if !option_affects_pane_terminal_geometry(option) {
        return Ok(Vec::new());
    }

    let (session_names, linked_refreshes) = match scope {
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal => (all_session_names(state), Vec::new()),
        OptionScopeSelector::Session(session_name) => (vec![session_name.clone()], Vec::new()),
        OptionScopeSelector::Window(target) => {
            linked_geometry_sessions(state, target.session_name(), target.window_index())
        }
        OptionScopeSelector::Pane(target) => {
            linked_geometry_sessions(state, target.session_name(), target.window_index())
        }
    };
    resize_terminals_for_sessions(state, session_names)?;
    Ok(linked_refreshes)
}

fn linked_geometry_sessions(
    state: &HandlerState,
    session_name: &SessionName,
    window_index: u32,
) -> (Vec<SessionName>, Vec<SessionName>) {
    let mut linked_sessions = state.window_linked_session_family_list(session_name, window_index);
    linked_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    linked_sessions.dedup();
    let linked_refreshes = linked_sessions
        .iter()
        .filter(|candidate| *candidate != session_name)
        .cloned()
        .collect();
    // Linked aliases share one runtime. Resize it once through the mutation
    // target, then redraw every other attached alias with its synchronized
    // option view.
    (vec![session_name.clone()], linked_refreshes)
}

fn option_affects_pane_terminal_geometry(option: OptionName) -> bool {
    matches!(
        option,
        OptionName::PaneBorderStatus
            | OptionName::PaneScrollbars
            | OptionName::PaneScrollbarsPosition
            | OptionName::PaneScrollbarsStyle
            | OptionName::Status
    )
}

fn all_session_names(state: &HandlerState) -> Vec<SessionName> {
    state
        .sessions
        .iter()
        .map(|(session_name, _)| session_name.clone())
        .collect()
}

fn resize_terminals_for_sessions(
    state: &mut HandlerState,
    session_names: Vec<SessionName>,
) -> Result<(), RmuxError> {
    for session_name in session_names {
        state.resize_terminals(&session_name)?;
    }
    Ok(())
}
