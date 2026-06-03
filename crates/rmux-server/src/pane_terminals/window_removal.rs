use std::collections::{HashMap, HashSet};

use rmux_core::{PaneId, Session};
use rmux_proto::{RmuxError, SessionName};

use super::super::{session_not_found, HandlerState, WindowLinkSlot};

#[derive(Debug, Clone)]
pub(super) struct WindowRemovalPlan {
    pub(super) session_name: SessionName,
    pub(super) window_index: u32,
    pub(super) runtime_session_name: SessionName,
    pub(super) pane_ids: Vec<PaneId>,
}

pub(super) fn build_window_removal_plan(
    state: &HandlerState,
    session: &Session,
    session_name: &SessionName,
    target_index: u32,
    kill_others: bool,
) -> Result<Vec<WindowRemovalPlan>, RmuxError> {
    window_pane_ids(session, session_name, target_index)?;

    let window_indices = if kill_others {
        session
            .windows()
            .keys()
            .copied()
            .filter(|window_index| *window_index != target_index)
            .collect::<Vec<_>>()
    } else {
        vec![target_index]
    };

    let mut seen_slots = HashSet::new();
    let mut removal_plan = Vec::new();
    for window_index in window_indices {
        let slots = expand_window_removal_slots(
            state,
            state.window_link_slots_for(session_name, window_index),
        );
        for slot in slots {
            if seen_slots.insert(slot.clone()) {
                removal_plan.push(build_window_slot_removal_plan(state, slot)?);
            }
        }
    }
    ensure_window_removal_leaves_survivors(state, &removal_plan)?;
    ensure_window_removal_terminals_exist(state, &removal_plan)?;
    Ok(removal_plan)
}

pub(in crate::pane_terminals) fn window_pane_ids(
    session: &Session,
    session_name: &SessionName,
    window_index: u32,
) -> Result<Vec<PaneId>, RmuxError> {
    let window = session.window_at(window_index).ok_or_else(|| {
        RmuxError::invalid_target(
            format!("{session_name}:{window_index}"),
            "window index does not exist in session",
        )
    })?;

    Ok(window.panes().iter().map(|pane| pane.id()).collect())
}

fn expand_window_removal_slots(
    state: &HandlerState,
    root_slots: Vec<WindowLinkSlot>,
) -> Vec<WindowLinkSlot> {
    let mut seen = HashSet::new();
    let mut expanded = Vec::new();
    let mut pending = root_slots;

    while let Some(slot) = pending.pop() {
        if !seen.insert(slot.clone()) {
            continue;
        }

        for member in state.sessions.session_group_members(&slot.session_name) {
            pending.push(WindowLinkSlot {
                session_name: member,
                window_index: slot.window_index,
            });
        }
        for linked_slot in state.window_link_slots_for(&slot.session_name, slot.window_index) {
            pending.push(linked_slot);
        }
        expanded.push(slot);
    }

    expanded
}

fn build_window_slot_removal_plan(
    state: &HandlerState,
    slot: WindowLinkSlot,
) -> Result<WindowRemovalPlan, RmuxError> {
    let session = state
        .sessions
        .session(&slot.session_name)
        .ok_or_else(|| session_not_found(&slot.session_name))?;
    Ok(WindowRemovalPlan {
        runtime_session_name: state
            .runtime_session_name_for_window(&slot.session_name, slot.window_index),
        pane_ids: window_pane_ids(session, &slot.session_name, slot.window_index)?,
        session_name: slot.session_name,
        window_index: slot.window_index,
    })
}

fn ensure_window_removal_leaves_survivors(
    state: &HandlerState,
    removal_plan: &[WindowRemovalPlan],
) -> Result<(), RmuxError> {
    let mut removals_by_session = HashMap::<SessionName, usize>::new();
    for planned_window in removal_plan {
        *removals_by_session
            .entry(planned_window.session_name.clone())
            .or_default() += 1;
    }

    for (session_name, removed_count) in removals_by_session {
        let session = state
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        if session.windows().len() <= removed_count {
            return Err(RmuxError::Server(format!(
                "cannot kill the only window in session {session_name}"
            )));
        }
    }

    Ok(())
}

fn ensure_window_removal_terminals_exist(
    state: &HandlerState,
    removal_plan: &[WindowRemovalPlan],
) -> Result<(), RmuxError> {
    let mut panes_by_runtime = HashMap::<SessionName, Vec<PaneId>>::new();
    for planned_window in removal_plan {
        panes_by_runtime
            .entry(planned_window.runtime_session_name.clone())
            .or_default()
            .extend(planned_window.pane_ids.iter().copied());
    }

    for (runtime_session_name, pane_ids) in panes_by_runtime {
        state
            .terminals
            .ensure_panes_exist(&runtime_session_name, &pane_ids)?;
    }

    Ok(())
}
