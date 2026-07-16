use rmux_core::{BreakPaneOptions, PaneJoinOptions, SessionPaneTarget};
use rmux_proto::{
    BreakPaneRequest, BreakPaneResponse, JoinPaneRequest, JoinPaneResponse, OptionScopeSelector,
    PaneTarget, RmuxError, SessionName, SetOptionMode, WindowTarget,
};

use super::super::{HandlerState, LinkedWindowTransferRemovalPlan, SessionTransferSnapshot};
use super::cross_session::transaction::{
    inserted_window_index_map, pane_option_snapshots_for_transfer,
    restore_pane_options_after_transfer, sync_pane_lifecycle_for_sessions,
    synchronize_cross_session_transfer_families, window_ids_by_index,
};
use super::window_metadata::PaneTransferWindowMetadata;
use super::{join_pane_internal_direction, pane_index_for_id};

impl HandlerState {
    pub(super) fn join_last_linked_pane_across_sessions(
        &mut self,
        request: JoinPaneRequest,
        source_pane_id: rmux_core::PaneId,
        removal_plan: LinkedWindowTransferRemovalPlan,
    ) -> Result<JoinPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let destination_session_name = request.target.session_name().clone();
        let source_pane_options = self.pane_explicit_option_entries(&request.source)?;
        let destination_family_options = pane_option_snapshots_for_transfer(
            self,
            &[(
                destination_session_name.clone(),
                request.target.window_index(),
            )],
        )?;
        let source_runtime = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        let destination_runtime = self.runtime_session_name_for_window(
            &destination_session_name,
            request.target.window_index(),
        );
        self.ensure_window_panes_exist(
            &source_session_name,
            request.source.window_index(),
            &[source_pane_id],
        )?;
        let snapshot = SessionTransferSnapshot::capture(self);

        let direction = join_pane_internal_direction(request.direction);
        let mutation = self.sessions.with_extracted_session_pair(
            &source_session_name,
            &destination_session_name,
            |source_session, destination_session| {
                destination_session.join_pane_from_session(
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
        if let Err(error) = mutation {
            snapshot.restore(self);
            return Err(error);
        }
        let moved_index = self
            .sessions
            .session(&destination_session_name)
            .and_then(|session| {
                pane_index_for_id(session, request.target.window_index(), source_pane_id)
            })
            .ok_or_else(|| {
                RmuxError::Server("moved pane disappeared after linked join-pane".to_owned())
            })?;
        let moved_target = PaneTarget::with_window(
            destination_session_name.clone(),
            request.target.window_index(),
            moved_index,
        );
        let destroyed_sessions = match self.commit_linked_last_pane_transfer_removal(removal_plan) {
            Ok(destroyed_sessions) => destroyed_sessions,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };
        let affected_sessions = match synchronize_cross_session_transfer_families(
            self,
            &[(
                destination_session_name.clone(),
                request.target.window_index(),
            )],
        ) {
            Ok(sessions) => sessions,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };
        if let Err(error) = restore_pane_options_after_transfer(self, &destination_family_options) {
            snapshot.restore(self);
            return Err(error);
        }

        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            snapshot.restore(self);
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            let terminal_rollback = self.terminals.move_panes_between_sessions(
                &destination_runtime,
                &source_runtime,
                &[source_pane_id],
            );
            snapshot.restore(self);
            terminal_rollback
                .map_err(|rollback_error| pane_transfer_rollback_error(&error, &rollback_error))?;
            return Err(error);
        }

        let commit_result = (|| {
            self.resize_terminals(&destination_session_name)?;
            sync_pane_lifecycle_for_sessions(self, &affected_sessions);
            self.restore_transferred_pane_options(&moved_target, &source_pane_options)
        })();
        if let Err(error) = commit_result {
            self.rollback_linked_last_pane_runtime(
                &source_runtime,
                &destination_runtime,
                source_pane_id,
                snapshot,
                &destination_session_name,
                &error,
            )?;
            return Err(error);
        }

        self.finish_destroyed_linked_session_transfers(destroyed_sessions)?;
        self.clear_marked_pane_if_id(source_pane_id);
        Ok(JoinPaneResponse {
            target: moved_target,
        })
    }

    pub(super) fn break_last_linked_pane_across_sessions(
        &mut self,
        request: BreakPaneRequest,
        destination_session_name: SessionName,
        source_pane_id: rmux_core::PaneId,
    ) -> Result<BreakPaneResponse, RmuxError> {
        let source_session_name = request.source.session_name().clone();
        let source_pane_options = self.pane_explicit_option_entries(&request.source)?;
        let source_window_target =
            WindowTarget::with_window(source_session_name.clone(), request.source.window_index());
        let source_window_metadata = PaneTransferWindowMetadata::capture(self, &request.source)?;
        let source_group_members = self.sessions.session_group_members(&source_session_name);
        let destination_slot_before = request
            .target
            .as_ref()
            .map_or(0, WindowTarget::window_index);
        let destination_family_options = pane_option_snapshots_for_transfer(
            self,
            &[(destination_session_name.clone(), destination_slot_before)],
        )?;
        let destination_window_ids_before = window_ids_by_index(self, &destination_session_name)?;
        let destination_group_members = self
            .sessions
            .session_group_members(&destination_session_name);
        let source_runtime = self
            .runtime_session_name_for_window(&source_session_name, request.source.window_index());
        self.ensure_window_panes_exist(
            &source_session_name,
            request.source.window_index(),
            &[source_pane_id],
        )?;
        let current_runtime_owner = self.sessions.runtime_owner(&source_session_name);
        let next_runtime_owner = (current_runtime_owner.as_ref() == Some(&source_session_name))
            .then(|| {
                self.sessions
                    .runtime_owner_transfer_target(&source_session_name)
            })
            .flatten();
        let snapshot = SessionTransferSnapshot::capture(self);

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
        let destination_index = match destination_index {
            Ok(destination_index) => destination_index,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };
        let source_session_will_be_removed = self
            .sessions
            .session(&source_session_name)
            .is_some_and(|session| session.windows().is_empty());
        let moved_target =
            PaneTarget::with_window(destination_session_name.clone(), destination_index, 0);
        let prepare_result = (|| {
            let destination_index_map = inserted_window_index_map(
                self,
                &destination_session_name,
                &destination_window_ids_before,
                destination_index,
            )?;
            for group_member in &destination_group_members {
                self.remap_reindexed_window_metadata(group_member, &destination_index_map)?;
            }
            self.synchronize_session_group_models_from_with_window_selection_map(
                &destination_session_name,
                &destination_index_map,
            )?;
            let destination_window =
                WindowTarget::with_window(destination_session_name.clone(), destination_index);
            source_window_metadata.move_to_surviving_window(self, &destination_window)?;
            self.move_window_link_slot(
                &source_session_name,
                request.source.window_index(),
                &destination_session_name,
                destination_index,
            );
            let affected_sessions = synchronize_cross_session_transfer_families(
                self,
                &[(destination_session_name.clone(), destination_index)],
            )?;
            restore_pane_options_after_transfer(self, &destination_family_options)?;
            Ok::<_, RmuxError>(affected_sessions)
        })();
        let affected_sessions = match prepare_result {
            Ok(sessions) => sessions,
            Err(error) => {
                snapshot.restore(self);
                return Err(error);
            }
        };
        let destination_runtime =
            self.runtime_session_name_for_window(&destination_session_name, destination_index);
        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            snapshot.restore(self);
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_runtime,
            &destination_runtime,
            &[source_pane_id],
        ) {
            let terminal_rollback = self.terminals.move_panes_between_sessions(
                &destination_runtime,
                &source_runtime,
                &[source_pane_id],
            );
            snapshot.restore(self);
            terminal_rollback
                .map_err(|rollback_error| pane_transfer_rollback_error(&error, &rollback_error))?;
            return Err(error);
        }

        let commit_result = (|| {
            self.resize_terminals(&destination_session_name)?;
            sync_pane_lifecycle_for_sessions(self, &affected_sessions);
            if source_session_will_be_removed {
                if source_group_members.len() > 1 {
                    self.remove_empty_source_session_group(source_group_members.clone())?;
                } else {
                    let _ = self.sessions.remove_session(&source_session_name)?;
                    let _ = self.options.remove_session(&source_session_name);
                    let _ = self.environment.remove_session(&source_session_name);
                    let _ = self.hooks.remove_session(&source_session_name);
                }
            } else {
                self.clear_auto_named_window(&source_session_name, request.source.window_index());
                let _ = self.options.remove_window(&source_window_target);
                let _ = self.hooks.remove_window(&source_window_target);
                self.synchronize_session_group_from(&source_session_name)?;
                self.resize_terminals(&source_session_name)?;
                self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
            }
            self.restore_transferred_pane_options(&moved_target, &source_pane_options)?;
            Ok(())
        })();
        if let Err(error) = commit_result {
            self.rollback_linked_last_pane_runtime(
                &source_runtime,
                &destination_runtime,
                source_pane_id,
                snapshot,
                &destination_session_name,
                &error,
            )?;
            return Err(error);
        }

        if source_session_will_be_removed && source_group_members.len() == 1 {
            self.remove_session_terminals(
                &source_session_name,
                current_runtime_owner.as_ref(),
                next_runtime_owner.as_ref(),
            )?;
        }
        source_window_metadata.prune_removed_aliases(self);
        self.clear_marked_pane_if_id(source_pane_id);
        Ok(BreakPaneResponse {
            target: moved_target,
            output: None,
        })
    }

    pub(in crate::pane_terminals) fn restore_transferred_pane_options(
        &mut self,
        target: &PaneTarget,
        entries: &[(String, String)],
    ) -> Result<(), RmuxError> {
        let _ = self.options.remove_pane(target);
        for (name, value) in entries {
            let _ = self.options.set_by_name(
                OptionScopeSelector::Pane(target.clone()),
                name,
                Some(value.clone()),
                SetOptionMode::Replace,
                false,
                false,
                false,
            )?;
        }
        self.synchronize_pane_alias_options_from_target(target)?;
        Ok(())
    }

    fn rollback_linked_last_pane_runtime(
        &mut self,
        source_runtime: &SessionName,
        destination_runtime: &SessionName,
        pane_id: rmux_core::PaneId,
        snapshot: SessionTransferSnapshot,
        resize_session_name: &SessionName,
        source_error: &RmuxError,
    ) -> Result<(), RmuxError> {
        let output_rollback = self.move_pane_outputs_between_sessions(
            destination_runtime,
            source_runtime,
            &[pane_id],
        );
        let terminal_rollback = self.terminals.move_panes_between_sessions(
            destination_runtime,
            source_runtime,
            &[pane_id],
        );
        snapshot.restore(self);
        let model_rollback = self.resize_terminals(resize_session_name);
        output_rollback.map_err(|rollback_error| {
            pane_transfer_rollback_error(source_error, &rollback_error)
        })?;
        terminal_rollback.map_err(|rollback_error| {
            pane_transfer_rollback_error(source_error, &rollback_error)
        })?;
        model_rollback
            .map_err(|rollback_error| pane_transfer_rollback_error(source_error, &rollback_error))
    }
}

fn pane_transfer_rollback_error(source: &RmuxError, rollback: &RmuxError) -> RmuxError {
    RmuxError::Server(format!(
        "failed to roll back linked pane transfer after {source}: {rollback}"
    ))
}
