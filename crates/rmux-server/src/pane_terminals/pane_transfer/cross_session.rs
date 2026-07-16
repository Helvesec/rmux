use rmux_core::{BreakPaneOptions, PaneJoinOptions, PaneSwapOptions, SessionPaneTarget};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, PaneTarget, RmuxError,
    SessionName, SwapPaneResponse, WindowTarget,
};

use super::super::{session_not_found, HandlerState};
use super::window_metadata::PaneTransferWindowMetadata;
use super::{join_pane_internal_direction, pane_id_for_target, pane_index_for_id};

#[path = "cross_session/transaction.rs"]
pub(super) mod transaction;
use transaction::{
    cross_session_rollback_error, inserted_window_index_map, pane_option_snapshots_for_transfer,
    resize_two_sessions, restore_pane_options_after_transfer, rollback_cross_session_move,
    rollback_cross_session_swap, sync_pane_lifecycle_for_sessions,
    synchronize_cross_session_transfer_families, window_ids_by_index, CrossSessionTransferSnapshot,
};

impl HandlerState {
    pub(super) fn swap_pane_across_sessions(
        &mut self,
        source: PaneTarget,
        target: PaneTarget,
        detached: bool,
        preserve_zoom: bool,
    ) -> Result<SwapPaneResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        let source_before_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let target_before_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let source_pane_id = pane_id_for_target(&source_before_session, &source)?;
        let target_pane_id = pane_id_for_target(&target_before_session, &target)?;
        let slots = [
            (source_session_name.clone(), source.window_index()),
            (target_session_name.clone(), target.window_index()),
        ];
        let pane_option_snapshots = pane_option_snapshots_for_transfer(self, &slots)?;
        let source_runtime =
            self.runtime_session_name_for_window(&source_session_name, source.window_index());
        let target_runtime =
            self.runtime_session_name_for_window(&target_session_name, target.window_index());
        self.ensure_window_panes_exist(
            &source_session_name,
            source.window_index(),
            &[source_pane_id],
        )?;
        self.ensure_window_panes_exist(
            &target_session_name,
            target.window_index(),
            &[target_pane_id],
        )?;
        let transfer_snapshot = CrossSessionTransferSnapshot::capture(self);

        let mutation_result = self.sessions.with_extracted_session_pair(
            &source_session_name,
            &target_session_name,
            |source_session, target_session| {
                source_session.swap_panes_with_session(
                    SessionPaneTarget::from(&source),
                    target_session,
                    SessionPaneTarget::from(&target),
                    PaneSwapOptions::new(detached, preserve_zoom),
                )
            },
        )?;
        if let Err(error) = mutation_result {
            transfer_snapshot.restore(self);
            return Err(error);
        }

        let lifecycle_sessions = match synchronize_cross_session_transfer_families(self, &slots) {
            Ok(sessions) => sessions,
            Err(error) => {
                transfer_snapshot.restore(self);
                return Err(error);
            }
        };
        if let Err(error) = restore_pane_options_after_transfer(self, &pane_option_snapshots) {
            transfer_snapshot.restore(self);
            return Err(error);
        }

        if let Err(error) = self.terminals.swap_panes_between_sessions(
            &source_runtime,
            &[source_pane_id],
            &target_runtime,
            &[target_pane_id],
        ) {
            transfer_snapshot.restore(self);
            return Err(error);
        }
        if let Err(error) = self.swap_pane_outputs_between_sessions(
            &source_runtime,
            &[source_pane_id],
            &target_runtime,
            &[target_pane_id],
        ) {
            let runtime_rollback = self.terminals.swap_panes_between_sessions(
                &source_runtime,
                &[target_pane_id],
                &target_runtime,
                &[source_pane_id],
            );
            transfer_snapshot.restore(self);
            runtime_rollback.map_err(|rollback_error| {
                cross_session_rollback_error(&error, &[rollback_error])
            })?;
            return Err(error);
        }

        if let Err(error) = resize_two_sessions(self, &source_session_name, &target_session_name) {
            rollback_cross_session_swap(
                self,
                transfer_snapshot,
                &source_session_name,
                &target_session_name,
                &source_runtime,
                source_pane_id,
                &target_runtime,
                target_pane_id,
                &error,
            )?;
            return Err(error);
        }

        sync_pane_lifecycle_for_sessions(self, &lifecycle_sessions);

        Ok(SwapPaneResponse { source, target })
    }

    pub(super) fn join_pane_across_sessions(
        &mut self,
        request: JoinPaneRequest,
    ) -> Result<JoinPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let target_session_name = request.target.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        if self.sessions.session(&target_session_name).is_none() {
            return Err(session_not_found(&target_session_name));
        }
        let current_runtime_owner = self.sessions.runtime_owner(&source_session_name);
        let next_runtime_owner = self
            .sessions
            .runtime_owner_transfer_target(&source_session_name);
        let source_group_members_before = self.sessions.session_group_members(&source_session_name);
        let source_pane_id = pane_id_for_target(&previous_source_session, &request.source)?;
        if let Some(removal_plan) =
            self.linked_last_pane_transfer_removal_plan(&request.source, source_pane_id)?
        {
            return self.join_last_linked_pane_across_sessions(
                request,
                source_pane_id,
                removal_plan,
            );
        }
        let source_window_metadata = PaneTransferWindowMetadata::capture(self, &request.source)?;
        let slots = [
            (source_session_name.clone(), request.source.window_index()),
            (target_session_name.clone(), request.target.window_index()),
        ];
        let pane_option_snapshots = pane_option_snapshots_for_transfer(self, &slots)?;
        let source_runtime = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        let target_runtime = self
            .runtime_session_name_for_window(&target_session_name, request.target.window_index());
        self.ensure_window_panes_exist(
            &source_session_name,
            request.source.window_index(),
            &[source_pane_id],
        )?;
        let transfer_snapshot = CrossSessionTransferSnapshot::capture(self);

        let direction = join_pane_internal_direction(request.direction);
        let mutation_result = self.sessions.with_extracted_session_pair(
            &source_session_name,
            &target_session_name,
            |source_session, target_session| {
                target_session.join_pane_from_session(
                    SessionPaneTarget::from(&request.target),
                    source_session,
                    SessionPaneTarget::from(&request.source),
                    PaneJoinOptions::new(
                        direction,
                        request.detached,
                        request.before,
                        request.full_size,
                        request.size,
                    ),
                )
            },
        )?;
        let source_session_will_be_removed = mutation_result.is_ok()
            && self
                .sessions
                .session(&source_session_name)
                .is_some_and(|session| session.windows().is_empty());
        if let Err(error) = mutation_result {
            transfer_snapshot.restore(self);
            return Err(error);
        }

        let mut synchronized_slots =
            vec![(target_session_name.clone(), request.target.window_index())];
        if !source_session_will_be_removed {
            synchronized_slots.push((source_session_name.clone(), request.source.window_index()));
        }
        let lifecycle_sessions =
            match synchronize_cross_session_transfer_families(self, &synchronized_slots) {
                Ok(sessions) => sessions,
                Err(error) => {
                    transfer_snapshot.restore(self);
                    return Err(error);
                }
            };
        let moved_target_result = (|| {
            let moved_index = self
                .sessions
                .session(&target_session_name)
                .and_then(|session| {
                    pane_index_for_id(session, request.target.window_index(), source_pane_id)
                })
                .ok_or_else(|| {
                    RmuxError::Server(
                        "moved pane disappeared after cross-session join-pane".to_owned(),
                    )
                })?;
            let moved_target = PaneTarget::with_window(
                target_session_name.clone(),
                request.target.window_index(),
                moved_index,
            );
            restore_pane_options_after_transfer(self, &pane_option_snapshots)?;
            Ok::<_, RmuxError>(moved_target)
        })();
        let moved_target = match moved_target_result {
            Ok(target) => target,
            Err(error) => {
                transfer_snapshot.restore(self);
                return Err(error);
            }
        };

        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_runtime,
            &target_runtime,
            &[source_pane_id],
        ) {
            transfer_snapshot.restore(self);
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_runtime,
            &target_runtime,
            &[source_pane_id],
        ) {
            let runtime_rollback = self.terminals.move_panes_between_sessions(
                &target_runtime,
                &source_runtime,
                &[source_pane_id],
            );
            transfer_snapshot.restore(self);
            runtime_rollback.map_err(|rollback_error| {
                cross_session_rollback_error(&error, &[rollback_error])
            })?;
            return Err(error);
        }

        let resize_result = if source_session_will_be_removed {
            self.resize_terminals(&target_session_name)
        } else {
            resize_two_sessions(self, &source_session_name, &target_session_name)
        };
        if let Err(error) = resize_result {
            rollback_cross_session_move(
                self,
                transfer_snapshot,
                &source_session_name,
                &target_session_name,
                &source_runtime,
                &target_runtime,
                source_pane_id,
                &error,
            )?;
            return Err(error);
        }

        if source_session_will_be_removed {
            if source_group_members_before.len() > 1 {
                self.remove_empty_source_session_group(source_group_members_before)?;
            } else {
                let _ = self.sessions.remove_session(&source_session_name)?;
                let _ = self.options.remove_session(&source_session_name);
                let _ = self.environment.remove_session(&source_session_name);
                let _ = self.hooks.remove_session(&source_session_name);
                self.remove_session_terminals(
                    &source_session_name,
                    current_runtime_owner.as_ref(),
                    next_runtime_owner.as_ref(),
                )?;
            }
        }

        source_window_metadata.prune_removed_aliases(self);
        sync_pane_lifecycle_for_sessions(self, &lifecycle_sessions);
        self.clear_marked_pane_if_id(source_pane_id);
        Ok(JoinPaneResponse {
            target: moved_target,
        })
    }

    pub(super) fn break_pane_across_sessions(
        &mut self,
        request: BreakPaneRequest,
        destination_session_name: SessionName,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        if self.sessions.session(&destination_session_name).is_none() {
            return Err(session_not_found(&destination_session_name));
        }
        let source_pane_id = pane_id_for_target(&previous_source_session, &request.source)?;
        if self
            .linked_last_pane_transfer_removal_plan(&request.source, source_pane_id)?
            .is_some()
        {
            return self.break_last_linked_pane_across_sessions(
                request,
                destination_session_name,
                source_pane_id,
            );
        }
        let source_window_metadata = PaneTransferWindowMetadata::capture(self, &request.source)?;
        let destination_slot_before = request
            .target
            .as_ref()
            .map_or(0, WindowTarget::window_index);
        let slots_before = [
            (source_session_name.clone(), request.source.window_index()),
            (destination_session_name.clone(), destination_slot_before),
        ];
        let pane_option_snapshots = pane_option_snapshots_for_transfer(self, &slots_before)?;
        let destination_window_ids_before = window_ids_by_index(self, &destination_session_name)?;
        let destination_group_members = self
            .sessions
            .session_group_members(&destination_session_name);
        let current_runtime_owner = self.sessions.runtime_owner(&source_session_name);
        let next_runtime_owner = self
            .sessions
            .runtime_owner_transfer_target(&source_session_name);
        let source_group_members_before = self.sessions.session_group_members(&source_session_name);
        let source_runtime = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        self.ensure_window_panes_exist(
            &source_session_name,
            request.source.window_index(),
            &[source_pane_id],
        )?;
        let transfer_snapshot = CrossSessionTransferSnapshot::capture(self);

        let destination_index = self.sessions.with_extracted_session_pair(
            &source_session_name,
            &destination_session_name,
            |source_session, destination_session| {
                source_session.break_pane_to_session(
                    SessionPaneTarget::from(&request.source),
                    destination_session,
                    BreakPaneOptions::new(
                        request.target.as_ref().map(WindowTarget::window_index),
                        request.name.clone(),
                        request.detached,
                        request.after,
                        request.before,
                    ),
                )
            },
        )?;
        let source_session_will_be_removed = destination_index.is_ok()
            && self
                .sessions
                .session(&source_session_name)
                .is_some_and(|session| session.windows().is_empty());
        let destination_index = match destination_index {
            Ok(destination_index) => destination_index,
            Err(error) => {
                transfer_snapshot.restore(self);
                return Err(error);
            }
        };
        let destination_index_map = match inserted_window_index_map(
            self,
            &destination_session_name,
            &destination_window_ids_before,
            destination_index,
        ) {
            Ok(index_map) => index_map,
            Err(error) => {
                transfer_snapshot.restore(self);
                return Err(error);
            }
        };
        for group_member in &destination_group_members {
            if let Err(error) =
                self.remap_reindexed_window_metadata(group_member, &destination_index_map)
            {
                transfer_snapshot.restore(self);
                return Err(error);
            }
        }
        if let Err(error) = self.synchronize_session_group_models_from_with_window_selection_map(
            &destination_session_name,
            &destination_index_map,
        ) {
            transfer_snapshot.restore(self);
            return Err(error);
        }
        let destination_window =
            WindowTarget::with_window(destination_session_name.clone(), destination_index);
        if let Err(error) =
            source_window_metadata.move_to_surviving_window(self, &destination_window)
        {
            transfer_snapshot.restore(self);
            return Err(error);
        }
        let destination_runtime =
            self.runtime_session_name_for_window(&destination_session_name, destination_index);

        let mut synchronized_slots = vec![(destination_session_name.clone(), destination_index)];
        if !source_session_will_be_removed {
            synchronized_slots.push((source_session_name.clone(), request.source.window_index()));
        }
        let lifecycle_sessions =
            match synchronize_cross_session_transfer_families(self, &synchronized_slots) {
                Ok(sessions) => sessions,
                Err(error) => {
                    transfer_snapshot.restore(self);
                    return Err(error);
                }
            };
        let moved_target =
            PaneTarget::with_window(destination_session_name.clone(), destination_index, 0);
        if let Err(error) = restore_pane_options_after_transfer(self, &pane_option_snapshots) {
            transfer_snapshot.restore(self);
            return Err(error);
        }

        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            transfer_snapshot.restore(self);
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            let runtime_rollback = self.terminals.move_panes_between_sessions(
                &destination_runtime,
                &source_runtime,
                &[source_pane_id],
            );
            transfer_snapshot.restore(self);
            runtime_rollback.map_err(|rollback_error| {
                cross_session_rollback_error(&error, &[rollback_error])
            })?;
            return Err(error);
        }

        let resize_result = if source_session_will_be_removed {
            self.resize_terminals(&destination_session_name)
        } else {
            resize_two_sessions(self, &source_session_name, &destination_session_name)
        };
        if let Err(error) = resize_result {
            rollback_cross_session_move(
                self,
                transfer_snapshot,
                &source_session_name,
                &destination_session_name,
                &source_runtime,
                &destination_runtime,
                source_pane_id,
                &error,
            )?;
            return Err(error);
        }

        if source_session_will_be_removed {
            if source_group_members_before.len() > 1 {
                self.remove_empty_source_session_group(source_group_members_before)?;
            } else {
                let _ = self.sessions.remove_session(&source_session_name)?;
                let _ = self.options.remove_session(&source_session_name);
                let _ = self.environment.remove_session(&source_session_name);
                let _ = self.hooks.remove_session(&source_session_name);
                self.remove_session_terminals(
                    &source_session_name,
                    current_runtime_owner.as_ref(),
                    next_runtime_owner.as_ref(),
                )?;
            }
        }

        source_window_metadata.prune_removed_aliases(self);
        sync_pane_lifecycle_for_sessions(self, &lifecycle_sessions);
        self.clear_marked_pane_if_id(source_pane_id);
        Ok(BreakPaneResponse {
            target: moved_target,
            output: None,
        })
    }
}
