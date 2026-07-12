use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rmux_core::{BreakPaneOptions, PaneId, PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, PaneTarget, RmuxError,
    SessionName, SwapPaneResponse, WindowTarget,
};

use super::super::{session_not_found, HandlerState};
use super::window_metadata::PaneTransferWindowMetadata;
use super::{join_pane_internal_direction, pane_id_for_target, pane_index_for_id};

impl HandlerState {
    pub(super) fn swap_pane_within_group(
        &mut self,
        source: PaneTarget,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let session_name = source.session_name().clone();
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let session = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        let source_pane_id = pane_id_for_target(session, &source)?;
        let target_pane_id = pane_id_for_target(session, &target)?;
        let source_runtime =
            self.runtime_session_name_for_window(source.session_name(), source.window_index());
        let target_runtime =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        self.ensure_window_panes_exist(
            source.session_name(),
            source.window_index(),
            &[source_pane_id],
        )?;
        self.ensure_window_panes_exist(
            target.session_name(),
            target.window_index(),
            &[target_pane_id],
        )?;

        let (response, ()) = self.mutate_session_transfer_and_resize_terminals(
            &session_name,
            |session| {
                session.swap_panes(
                    SessionPaneTarget::from(&source),
                    SessionPaneTarget::from(&target),
                    PaneSwapOptions::new(detached, preserve_zoom),
                )?;
                Ok(SwapPaneResponse {
                    source: source.clone(),
                    target: target.clone(),
                })
            },
            |state, _| {
                move_grouped_runtime_swap(
                    state,
                    &source_runtime,
                    source_pane_id,
                    &target_runtime,
                    target_pane_id,
                )
            },
            |state, _| {
                rollback_grouped_runtime_swap(
                    state,
                    &source_runtime,
                    source_pane_id,
                    &target_runtime,
                    target_pane_id,
                )
            },
            |state, _| {
                state.synchronize_session_group_from(&session_name)?;
                state.sync_pane_lifecycle_dimensions_for_session(&session_name);
                state.synchronize_grouped_transfer_windows(
                    &session_name,
                    None,
                    &[source.window_index(), target.window_index()],
                )?;
                state.rekey_and_synchronize_pane_options_after_session_change(
                    &before_pane_options,
                    &session_name,
                )
            },
        )?;
        Ok(response)
    }

    pub(super) fn join_pane_within_group(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        let session_name = request.target.session_name().clone();
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let source_pane_options = self.pane_explicit_option_entries(&request.source)?;
        let source_window_metadata = PaneTransferWindowMetadata::capture(self, &request.source)?;
        let session = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        let moved_pane_id = pane_id_for_target(session, &request.source)?;
        let source_runtime = self.runtime_session_name_for_window(
            request.source.session_name(),
            request.source.window_index(),
        );
        let target_runtime = self.runtime_session_name_for_window(
            request.target.session_name(),
            request.target.window_index(),
        );
        self.ensure_window_panes_exist(
            request.source.session_name(),
            request.source.window_index(),
            &[moved_pane_id],
        )?;
        let linked_removal =
            self.linked_last_pane_transfer_removal_plan(&request.source, moved_pane_id)?;

        let target = request.target.clone();
        let direction = join_pane_internal_direction(request.direction);
        let (response, destroyed_sessions) = self.mutate_session_transfer_and_resize_terminals(
            &session_name,
            |session| {
                session.join_pane(
                    SessionPaneTarget::from(&request.source),
                    SessionPaneTarget::from(&request.target),
                    PaneJoinOptions::new(
                        direction,
                        request.detached,
                        request.before,
                        request.full_size,
                        request.size,
                    ),
                )?;
                let moved_index = pane_index_for_id(session, target.window_index(), moved_pane_id)
                    .ok_or_else(|| {
                        RmuxError::Server("moved pane disappeared after join-pane".to_owned())
                    })?;
                Ok(JoinPaneResponse {
                    target: PaneTarget::with_window(
                        session_name.clone(),
                        target.window_index(),
                        moved_index,
                    ),
                })
            },
            |state, _| {
                move_grouped_runtime_pane(state, &source_runtime, &target_runtime, moved_pane_id)
            },
            |state, _| {
                rollback_grouped_runtime_pane(
                    state,
                    &source_runtime,
                    &target_runtime,
                    moved_pane_id,
                )
            },
            |state, response| {
                let destroyed_sessions = match linked_removal {
                    Some(plan) => state.commit_linked_last_pane_transfer_removal(plan)?,
                    None => Vec::new(),
                };
                state.synchronize_session_group_from(&session_name)?;
                state.sync_pane_lifecycle_dimensions_for_session(&session_name);
                state.synchronize_grouped_transfer_windows(
                    &session_name,
                    None,
                    &[request.source.window_index(), request.target.window_index()],
                )?;
                state.rekey_and_synchronize_pane_options_after_session_change(
                    &before_pane_options,
                    &session_name,
                )?;
                state.restore_transferred_pane_options(&response.target, &source_pane_options)?;
                source_window_metadata.prune_removed_aliases(state);
                Ok(destroyed_sessions)
            },
        )?;
        self.finish_destroyed_linked_session_transfers(destroyed_sessions)?;
        self.clear_marked_pane_if_id(moved_pane_id);
        Ok(response)
    }

    pub(super) fn break_pane_within_group(
        &mut self,
        request: BreakPaneRequest,
        destination_session_name: SessionName,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let session_name = destination_session_name.clone();
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let source_pane_options = self.pane_explicit_option_entries(&request.source)?;
        let session = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        let before_window_ids = window_ids_by_index(session);
        let source_window_is_single_pane = session
            .window_at(request.source.window_index())
            .is_some_and(|window| window.pane_count() == 1);
        let group_members = self.sessions.session_group_members(&session_name);
        let moved_pane_id = pane_id_for_target(session, &request.source)?;
        let source_runtime = self.runtime_session_name_for_window(
            request.source.session_name(),
            request.source.window_index(),
        );
        self.ensure_window_panes_exist(
            request.source.session_name(),
            request.source.window_index(),
            &[moved_pane_id],
        )?;
        let (response, ()) = self.mutate_session_transfer_and_resize_terminals(
            &session_name,
            |session| {
                let destination_index = session.break_pane(
                    SessionPaneTarget::from(&request.source),
                    BreakPaneOptions::new(
                        request.target.as_ref().map(WindowTarget::window_index),
                        request.name.clone(),
                        request.detached,
                        request.after,
                        request.before,
                    ),
                )?;
                Ok(BreakPaneResponse {
                    target: PaneTarget::with_window(
                        destination_session_name.clone(),
                        destination_index,
                        0,
                    ),
                    output: None,
                })
            },
            |state, response| {
                let index_map = break_window_index_map(
                    state,
                    &session_name,
                    &before_window_ids,
                    request.source.window_index(),
                    source_window_is_single_pane,
                    response.target.window_index(),
                )?;
                for group_member in &group_members {
                    state.remap_reindexed_window_metadata(group_member, &index_map)?;
                }
                let destination_runtime = state.runtime_session_name_for_window(
                    response.target.session_name(),
                    response.target.window_index(),
                );
                move_grouped_runtime_pane(
                    state,
                    &source_runtime,
                    &destination_runtime,
                    moved_pane_id,
                )
            },
            |state, response| {
                let destination_runtime = state.runtime_session_name_for_window(
                    response.target.session_name(),
                    response.target.window_index(),
                );
                rollback_grouped_runtime_pane(
                    state,
                    &source_runtime,
                    &destination_runtime,
                    moved_pane_id,
                )
            },
            |state, response| {
                let index_map = break_window_index_map(
                    state,
                    &session_name,
                    &before_window_ids,
                    request.source.window_index(),
                    source_window_is_single_pane,
                    response.target.window_index(),
                )?;
                let reindexed_before_pane_options =
                    remap_pane_slot_windows(&before_pane_options, &index_map);
                let selection_map = if source_window_is_single_pane {
                    BTreeMap::from([(
                        request.source.window_index(),
                        response.target.window_index(),
                    )])
                } else {
                    BTreeMap::new()
                };
                state.synchronize_session_group_from_with_window_selection_and_winlink_alert_maps(
                    &session_name,
                    &selection_map,
                    &index_map,
                )?;
                state.sync_pane_lifecycle_dimensions_for_session(&session_name);
                let mut affected_window_indexes = index_map.values().copied().collect::<Vec<_>>();
                affected_window_indexes.push(response.target.window_index());
                let mutated_source_window_index = index_map
                    .get(&request.source.window_index())
                    .copied()
                    .unwrap_or_else(|| request.source.window_index());
                state.synchronize_grouped_window_options_from(
                    &session_name,
                    &affected_window_indexes,
                );
                state.synchronize_grouped_transfer_windows(
                    &session_name,
                    Some(mutated_source_window_index),
                    &affected_window_indexes,
                )?;
                state.rekey_and_synchronize_pane_options_after_session_change(
                    &reindexed_before_pane_options,
                    &session_name,
                )?;
                state.restore_transferred_pane_options(&response.target, &source_pane_options)?;
                Ok(())
            },
        )?;
        self.clear_marked_pane_if_id(moved_pane_id);
        Ok(response)
    }

    fn synchronize_grouped_transfer_windows(
        &mut self,
        session_name: &SessionName,
        source_window_index: Option<u32>,
        window_indexes: &[u32],
    ) -> Result<(), RmuxError> {
        let mut affected_sessions = HashSet::new();
        let mut ordered_indexes = window_indexes.iter().copied().collect::<BTreeSet<_>>();
        let source_window_index =
            source_window_index.filter(|window_index| ordered_indexes.remove(window_index));
        for window_index in source_window_index.into_iter().chain(ordered_indexes) {
            if self
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(window_index))
                .is_none()
            {
                continue;
            }
            affected_sessions.extend(
                self.synchronize_linked_window_family_from_slot(session_name, window_index)?,
            );
        }
        for affected_session in affected_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(&affected_session);
        }
        Ok(())
    }

    fn synchronize_grouped_window_options_from(
        &mut self,
        session_name: &SessionName,
        window_indexes: &[u32],
    ) {
        let group_members = self.sessions.session_group_members(session_name);
        for window_index in window_indexes.iter().copied().collect::<BTreeSet<_>>() {
            if self
                .sessions
                .session(session_name)
                .and_then(|session| session.window_at(window_index))
                .is_none()
            {
                continue;
            }
            let source = WindowTarget::with_window(session_name.clone(), window_index);
            for group_member in &group_members {
                if group_member == session_name {
                    continue;
                }
                let target = WindowTarget::with_window(group_member.clone(), window_index);
                self.options.copy_window_overrides(&source, &target);
            }
            self.synchronize_linked_window_options_from_slot(session_name, window_index);
        }
    }
}

fn window_ids_by_index(session: &rmux_core::Session) -> BTreeMap<u32, u32> {
    session
        .windows()
        .iter()
        .map(|(window_index, window)| (*window_index, window.id().as_u32()))
        .collect()
}

fn break_window_index_map(
    state: &HandlerState,
    session_name: &SessionName,
    before: &BTreeMap<u32, u32>,
    source_window_index: u32,
    source_window_is_single_pane: bool,
    destination_window_index: u32,
) -> Result<BTreeMap<u32, u32>, RmuxError> {
    let session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let after = window_ids_by_index(session);
    let mut old_by_id = window_indexes_by_id(before);
    let mut new_by_id = window_indexes_by_id(&after);
    let mut index_map = BTreeMap::new();

    if source_window_is_single_pane {
        let source_window_id = before.get(&source_window_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "source window index {source_window_index} disappeared before break-pane remap"
            ))
        })?;
        let destination_window_id = after.get(&destination_window_index).ok_or_else(|| {
            RmuxError::Server(format!(
                "destination window index {destination_window_index} disappeared during break-pane remap"
            ))
        })?;
        if source_window_id != destination_window_id {
            return Err(RmuxError::Server(format!(
                "break-pane destination window @{destination_window_id} does not match moved source window @{source_window_id}"
            )));
        }
        index_map.insert(source_window_index, destination_window_index);
        remove_window_index_occurrence(&mut old_by_id, *source_window_id, source_window_index)?;
        remove_window_index_occurrence(
            &mut new_by_id,
            *source_window_id,
            destination_window_index,
        )?;
    }

    for (window_id, old_indexes) in old_by_id {
        let new_indexes = new_by_id.remove(&window_id).unwrap_or_default();
        if old_indexes.len() != new_indexes.len() {
            return Err(RmuxError::Server(format!(
                "window @{window_id} occurrence count changed during break-pane remap"
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

fn remove_window_index_occurrence(
    indexes: &mut BTreeMap<u32, Vec<u32>>,
    window_id: u32,
    window_index: u32,
) -> Result<(), RmuxError> {
    let candidates = indexes.get_mut(&window_id).ok_or_else(|| {
        RmuxError::Server(format!(
            "window @{window_id} has no occurrence at index {window_index}"
        ))
    })?;
    let position = candidates
        .iter()
        .position(|candidate| *candidate == window_index)
        .ok_or_else(|| {
            RmuxError::Server(format!(
                "window @{window_id} has no occurrence at index {window_index}"
            ))
        })?;
    candidates.remove(position);
    if candidates.is_empty() {
        indexes.remove(&window_id);
    }
    Ok(())
}

fn remap_pane_slot_windows(
    before: &HashMap<PaneId, Vec<PaneTarget>>,
    index_map: &BTreeMap<u32, u32>,
) -> HashMap<PaneId, Vec<PaneTarget>> {
    before
        .iter()
        .map(|(pane_id, targets)| {
            let targets = targets
                .iter()
                .map(|target| {
                    let window_index = index_map
                        .get(&target.window_index())
                        .copied()
                        .unwrap_or_else(|| target.window_index());
                    PaneTarget::with_window(
                        target.session_name().clone(),
                        window_index,
                        target.pane_index(),
                    )
                })
                .collect();
            (*pane_id, targets)
        })
        .collect()
}

fn move_grouped_runtime_pane(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    destination_runtime: &SessionName,
    pane_id: PaneId,
) -> Result<(), RmuxError> {
    state
        .terminals
        .move_panes_between_sessions(source_runtime, destination_runtime, &[pane_id])?;
    if let Err(error) =
        state.move_pane_outputs_between_sessions(source_runtime, destination_runtime, &[pane_id])
    {
        state.terminals.move_panes_between_sessions(
            destination_runtime,
            source_runtime,
            &[pane_id],
        )?;
        return Err(error);
    }
    Ok(())
}

fn rollback_grouped_runtime_pane(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    destination_runtime: &SessionName,
    pane_id: PaneId,
) -> Result<(), RmuxError> {
    let output_result =
        state.move_pane_outputs_between_sessions(destination_runtime, source_runtime, &[pane_id]);
    let terminal_result = state.terminals.move_panes_between_sessions(
        destination_runtime,
        source_runtime,
        &[pane_id],
    );
    output_result?;
    terminal_result
}

fn move_grouped_runtime_swap(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    source_pane_id: PaneId,
    target_runtime: &SessionName,
    target_pane_id: PaneId,
) -> Result<(), RmuxError> {
    state.terminals.swap_panes_between_sessions(
        source_runtime,
        &[source_pane_id],
        target_runtime,
        &[target_pane_id],
    )?;
    if let Err(error) = state.swap_pane_outputs_between_sessions(
        source_runtime,
        &[source_pane_id],
        target_runtime,
        &[target_pane_id],
    ) {
        state.terminals.swap_panes_between_sessions(
            source_runtime,
            &[target_pane_id],
            target_runtime,
            &[source_pane_id],
        )?;
        return Err(error);
    }
    Ok(())
}

fn rollback_grouped_runtime_swap(
    state: &mut HandlerState,
    source_runtime: &SessionName,
    source_pane_id: PaneId,
    target_runtime: &SessionName,
    target_pane_id: PaneId,
) -> Result<(), RmuxError> {
    let output_result = state.swap_pane_outputs_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    let terminal_result = state.terminals.swap_panes_between_sessions(
        source_runtime,
        &[target_pane_id],
        target_runtime,
        &[source_pane_id],
    );
    output_result?;
    terminal_result
}
