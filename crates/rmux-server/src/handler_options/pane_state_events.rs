use rmux_core::{OptionMutationOutcome, PaneId};
use rmux_proto::{OptionScopeSelector, PaneTarget, RmuxError};

pub(super) fn pane_id_for_resolved_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
) -> Result<rmux_proto::PaneId, RmuxError> {
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .and_then(|window| window.pane(target.pane_index()))
        .map(|pane| pane.id())
        .ok_or_else(|| {
            RmuxError::invalid_target(target.to_string(), "pane index does not exist in session")
        })
}

pub(super) fn pane_option_events_for_outcome(
    state: &crate::pane_terminals::HandlerState,
    outcome: &OptionMutationOutcome,
) -> Vec<(PaneId, u64, OptionMutationOutcome)> {
    let mut events = Vec::new();
    collect_pane_option_events(state, outcome, &mut events);
    events
}

pub(super) fn synchronize_pane_option_aliases_for_outcome(
    state: &mut crate::pane_terminals::HandlerState,
    outcome: &OptionMutationOutcome,
) -> Result<(), RmuxError> {
    if outcome.changed {
        if let OptionScopeSelector::Pane(target) = &outcome.scope {
            state.synchronize_pane_alias_options_from_target(target)?;
        }
    }
    for related in &outcome.related {
        synchronize_pane_option_aliases_for_outcome(state, related)?;
    }
    Ok(())
}

fn collect_pane_option_events(
    state: &crate::pane_terminals::HandlerState,
    outcome: &OptionMutationOutcome,
    events: &mut Vec<(PaneId, u64, OptionMutationOutcome)>,
) {
    if let Some(event) = pane_option_event_for_option_scope(state, &outcome.scope, outcome) {
        events.push(event);
    }
    for related in &outcome.related {
        collect_pane_option_events(state, related, events);
    }
}

fn pane_option_event_for_option_scope(
    state: &crate::pane_terminals::HandlerState,
    scope: &OptionScopeSelector,
    outcome: &OptionMutationOutcome,
) -> Option<(PaneId, u64, OptionMutationOutcome)> {
    match scope {
        OptionScopeSelector::Pane(target) => pane_option_event_for_target(state, target, outcome),
        OptionScopeSelector::ServerGlobal
        | OptionScopeSelector::SessionGlobal
        | OptionScopeSelector::WindowGlobal
        | OptionScopeSelector::Session(_)
        | OptionScopeSelector::Window(_) => None,
    }
}

fn pane_option_event_for_target(
    state: &crate::pane_terminals::HandlerState,
    target: &PaneTarget,
    outcome: &OptionMutationOutcome,
) -> Option<(PaneId, u64, OptionMutationOutcome)> {
    if !outcome.changed {
        return None;
    }
    let pane_id = pane_id_for_resolved_target(state, target).ok()?;
    let generation = state.pane_output_generation(target.session_name(), pane_id);
    Some((pane_id, generation, outcome.clone()))
}
