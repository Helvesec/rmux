use std::collections::{BTreeMap, HashMap, HashSet};

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use super::super::super::{
    HandlerState, PaneExitMetadata, PaneLifecycleState, SessionTransferSnapshot,
};

pub(super) struct CrossSessionTransferSnapshot {
    model: SessionTransferSnapshot,
    pane_lifecycle: HashMap<PaneId, PaneLifecycleState>,
    dead_panes: HashMap<SessionName, HashMap<PaneId, PaneExitMetadata>>,
}

impl CrossSessionTransferSnapshot {
    pub(super) fn capture(state: &HandlerState) -> Self {
        Self {
            model: SessionTransferSnapshot::capture(state),
            pane_lifecycle: state.pane_lifecycle.clone(),
            dead_panes: state.dead_panes.clone(),
        }
    }

    pub(super) fn restore(self, state: &mut HandlerState) {
        let (pane_lifecycle, dead_panes) = self.restore_model(state);
        state.pane_lifecycle = pane_lifecycle;
        state.dead_panes = dead_panes;
    }

    fn restore_model(
        self,
        state: &mut HandlerState,
    ) -> (
        HashMap<PaneId, PaneLifecycleState>,
        HashMap<SessionName, HashMap<PaneId, PaneExitMetadata>>,
    ) {
        self.model.restore(state);
        (self.pane_lifecycle, self.dead_panes)
    }
}

pub(in crate::pane_terminals::pane_transfer) struct PaneOptionSnapshots {
    slots: Vec<(SessionName, Vec<(PaneId, PaneTarget)>)>,
    entries: HashMap<PaneId, Vec<(String, String)>>,
}

pub(in crate::pane_terminals::pane_transfer) fn pane_option_snapshots_for_transfer(
    state: &HandlerState,
    slots: &[(SessionName, u32)],
) -> Result<PaneOptionSnapshots, RmuxError> {
    let slots = transfer_family_session_names(state, slots)
        .into_iter()
        .filter(|session_name| state.sessions.session(session_name).is_some())
        .map(|session_name| {
            pane_slots_for_session(state, &session_name).map(|snapshot| (session_name, snapshot))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut entries = HashMap::new();
    for (_, snapshot) in &slots {
        for (pane_id, target) in snapshot {
            if !entries.contains_key(pane_id) {
                entries.insert(*pane_id, state.pane_explicit_option_entries(target)?);
            }
        }
    }
    Ok(PaneOptionSnapshots { slots, entries })
}

pub(in crate::pane_terminals::pane_transfer) fn restore_pane_options_after_transfer(
    state: &mut HandlerState,
    snapshots: &PaneOptionSnapshots,
) -> Result<(), RmuxError> {
    let mut affected_targets = HashSet::new();
    for (session_name, before) in &snapshots.slots {
        affected_targets.extend(before.iter().map(|(_, target)| target.clone()));
        if state.sessions.session(session_name).is_some() {
            affected_targets.extend(
                pane_slots_for_session(state, session_name)?
                    .into_iter()
                    .map(|(_, target)| target),
            );
        }
    }
    for target in affected_targets {
        let _ = state.options.remove_pane(&target);
    }

    let mut entries = snapshots.entries.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(pane_id, _)| pane_id.as_u32());
    for (pane_id, entries) in entries {
        let Some(target) = state.pane_alias_targets(*pane_id).into_iter().next() else {
            continue;
        };
        state.restore_transferred_pane_options(&target, entries)?;
    }
    Ok(())
}

fn pane_slots_for_session(
    state: &HandlerState,
    session_name: &SessionName,
) -> Result<Vec<(PaneId, PaneTarget)>, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| super::super::super::session_not_found(session_name))?;
    Ok(session
        .windows()
        .iter()
        .flat_map(|(window_index, window)| {
            window.panes().iter().map(move |pane| {
                (
                    pane.id(),
                    PaneTarget::with_window(session_name.clone(), *window_index, pane.index()),
                )
            })
        })
        .collect())
}

pub(in crate::pane_terminals::pane_transfer) fn synchronize_cross_session_transfer_families(
    state: &mut HandlerState,
    slots: &[(SessionName, u32)],
) -> Result<Vec<SessionName>, RmuxError> {
    let mut affected_sessions = HashSet::new();
    for (session_name, _) in slots {
        if state.sessions.session(session_name).is_some() {
            affected_sessions.extend(state.synchronize_session_group_from(session_name)?);
        }
    }
    for (session_name, window_index) in slots {
        if state
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(*window_index))
            .is_some()
        {
            affected_sessions.extend(
                state.synchronize_linked_window_family_from_slot(session_name, *window_index)?,
            );
        }
    }
    let mut affected_sessions = affected_sessions.into_iter().collect::<Vec<_>>();
    affected_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(affected_sessions)
}

pub(super) fn transfer_family_session_names(
    state: &HandlerState,
    slots: &[(SessionName, u32)],
) -> Vec<SessionName> {
    let mut session_names = HashSet::new();
    for (session_name, window_index) in slots {
        session_names.extend(state.window_linked_session_family_list(session_name, *window_index));
    }
    let mut session_names = session_names.into_iter().collect::<Vec<_>>();
    session_names.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    session_names
}

pub(in crate::pane_terminals::pane_transfer) fn window_ids_by_index(
    state: &HandlerState,
    session_name: &SessionName,
) -> Result<BTreeMap<u32, u32>, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| super::super::super::session_not_found(session_name))?;
    Ok(session
        .windows()
        .iter()
        .map(|(window_index, window)| (*window_index, window.id().as_u32()))
        .collect())
}

pub(in crate::pane_terminals::pane_transfer) fn inserted_window_index_map(
    state: &HandlerState,
    session_name: &SessionName,
    before: &BTreeMap<u32, u32>,
    inserted_window_index: u32,
) -> Result<BTreeMap<u32, u32>, RmuxError> {
    let mut after = window_ids_by_index(state, session_name)?;
    let _ = after.remove(&inserted_window_index);
    let old_by_id = window_indexes_by_id(before);
    let new_by_id = window_indexes_by_id(&after);
    let mut index_map = BTreeMap::new();
    for (window_id, old_indexes) in old_by_id {
        let new_indexes = new_by_id.get(&window_id).cloned().unwrap_or_default();
        if old_indexes.len() != new_indexes.len() {
            return Err(RmuxError::Server(format!(
                "window @{window_id} occurrence count changed during cross-session break-pane"
            )));
        }
        index_map.extend(old_indexes.into_iter().zip(new_indexes));
    }
    Ok(index_map)
}

fn window_indexes_by_id(indexes: &BTreeMap<u32, u32>) -> BTreeMap<u32, Vec<u32>> {
    let mut by_id = BTreeMap::<u32, Vec<u32>>::new();
    for (window_index, window_id) in indexes {
        by_id.entry(*window_id).or_default().push(*window_index);
    }
    by_id
}

pub(in crate::pane_terminals::pane_transfer) fn sync_pane_lifecycle_for_sessions(
    state: &mut HandlerState,
    sessions: &[SessionName],
) {
    for session_name in sessions {
        state.sync_pane_lifecycle_dimensions_for_session(session_name);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rollback_cross_session_move(
    state: &mut HandlerState,
    snapshot: CrossSessionTransferSnapshot,
    source_session_name: &SessionName,
    target_session_name: &SessionName,
    source_runtime: &SessionName,
    target_runtime: &SessionName,
    pane_id: PaneId,
    source_error: &RmuxError,
) -> Result<(), RmuxError> {
    let output_rollback =
        state.move_pane_outputs_between_sessions(target_runtime, source_runtime, &[pane_id]);
    let terminal_rollback =
        state
            .terminals
            .move_panes_between_sessions(target_runtime, source_runtime, &[pane_id]);
    let (pane_lifecycle, dead_panes) = snapshot.restore_model(state);
    let resize_rollback = resize_two_sessions(state, source_session_name, target_session_name);
    state.pane_lifecycle = pane_lifecycle;
    state.dead_panes = dead_panes;
    cross_session_rollback_result(
        source_error,
        [output_rollback, terminal_rollback, resize_rollback],
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn rollback_cross_session_swap(
    state: &mut HandlerState,
    snapshot: CrossSessionTransferSnapshot,
    source_session_name: &SessionName,
    target_session_name: &SessionName,
    source_runtime: &SessionName,
    source_pane_id: PaneId,
    target_runtime: &SessionName,
    target_pane_id: PaneId,
    source_error: &RmuxError,
) -> Result<(), RmuxError> {
    let output_rollback = state.swap_pane_outputs_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    let terminal_rollback = state.terminals.swap_panes_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    let (pane_lifecycle, dead_panes) = snapshot.restore_model(state);
    let resize_rollback = resize_two_sessions(state, source_session_name, target_session_name);
    state.pane_lifecycle = pane_lifecycle;
    state.dead_panes = dead_panes;
    cross_session_rollback_result(
        source_error,
        [output_rollback, terminal_rollback, resize_rollback],
    )
}

fn cross_session_rollback_result<const N: usize>(
    source_error: &RmuxError,
    results: [Result<(), RmuxError>; N],
) -> Result<(), RmuxError> {
    let failures = results
        .into_iter()
        .filter_map(Result::err)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if failures.is_empty() {
        return Ok(());
    }
    Err(RmuxError::Server(format!(
        "failed to roll back cross-session pane transfer after {source_error}: {}",
        failures.join("; ")
    )))
}

pub(super) fn cross_session_rollback_error(
    source: &RmuxError,
    failures: &[RmuxError],
) -> RmuxError {
    RmuxError::Server(format!(
        "failed to roll back cross-session pane transfer after {source}: {}",
        failures
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

pub(super) fn resize_two_sessions(
    state: &mut HandlerState,
    source_session_name: &SessionName,
    target_session_name: &SessionName,
) -> Result<(), RmuxError> {
    state.resize_terminals(source_session_name)?;
    state.resize_terminals(target_session_name)
}
