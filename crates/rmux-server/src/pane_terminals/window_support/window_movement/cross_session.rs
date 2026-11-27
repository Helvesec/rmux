use std::collections::{BTreeMap, HashMap};

use rmux_proto::{MoveWindowResponse, RmuxError, SessionName, WindowTarget};

use super::super::{session_not_found, window_pane_ids, HandlerState};

mod linked;
mod swap;

impl HandlerState {
    pub(super) fn move_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        kill_destination: bool,
        detached: bool,
        target_group_index_map: Option<&BTreeMap<u32, u32>>,
    ) -> Result<MoveWindowResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        self.reject_window_move_between_grouped_sessions(
            &source_session_name,
            &target_session_name,
        )?;
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_window_link_occurrences = self.window_link_occurrences.clone();
        let source_was_last_window = previous_source_session.windows().count() == 1;
        let source_group_members_before = if source_was_last_window {
            self.sessions.session_group_members(&source_session_name)
        } else {
            Vec::new()
        };
        let source_window = previous_source_session
            .window_at(source.window_index())
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    source.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let source_alert_flags = previous_source_session.winlink_alert_flags(source.window_index());
        let target_exists = previous_target_session
            .window_at(target.window_index())
            .is_some();
        if target_exists && !kill_destination {
            return Err(RmuxError::Server(format!(
                "index in use: {}",
                target.window_index()
            )));
        }
        if self.window_link_count(&source_session_name, source.window_index()) > 1 {
            return self.move_linked_window_across_sessions(
                source,
                target,
                kill_destination,
                detached,
                target_group_index_map,
                self.sessions.checkpoint_structure(&[
                    source_session_name.clone(),
                    target_session_name.clone(),
                ])?,
                previous_options,
                previous_hooks,
            );
        }

        let source_pane_ids = window_pane_ids(
            previous_source_session,
            &source_session_name,
            source.window_index(),
        )?;
        let source_runtime_before =
            self.runtime_session_name_for_window(&source_session_name, source.window_index());
        let target_runtime_before = if target_exists {
            self.runtime_session_name_for_window(&target_session_name, target.window_index())
        } else {
            self.runtime_session_name(&target_session_name)
        };
        let replaced_target_pane_ids = if target_exists && kill_destination {
            window_pane_ids(
                previous_target_session,
                &target_session_name,
                target.window_index(),
            )?
        } else {
            Vec::new()
        };
        let target_link_runtime_transfer_slot = if target_exists
            && kill_destination
            && self.window_link_count(&target_session_name, target.window_index()) > 1
        {
            self.linked_runtime_transfer_slot_for_detached_window(
                &target_session_name,
                target.window_index(),
            )
        } else {
            None
        };
        let removed_target_pane_ids = if target_link_runtime_transfer_slot.is_some() {
            Vec::new()
        } else {
            replaced_target_pane_ids.clone()
        };
        self.terminals
            .ensure_panes_exist(&source_runtime_before, &source_pane_ids)?;
        self.terminals
            .ensure_panes_exist(&target_runtime_before, &[])?;
        if target_link_runtime_transfer_slot.is_some() {
            self.terminals
                .ensure_panes_exist(&target_runtime_before, &replaced_target_pane_ids)?;
        } else if !removed_target_pane_ids.is_empty() {
            self.terminals
                .ensure_panes_exist(&target_runtime_before, &removed_target_pane_ids)?;
        }

        let source_window_id = source_window.id();
        let mut previous_source_session = self
            .sessions
            .checkpoint_structure(&[source_session_name.clone(), target_session_name.clone()])?;
        let change = self.sessions.install_window_alias(
            &target,
            source_window_id,
            kill_destination,
            false,
        )?;
        previous_source_session.retain_removed(change.removed_windows);
        self.sessions
            .replace_winlink_alert_flags(&target, source_alert_flags)?;
        if !source_was_last_window {
            match self.sessions.remove_window(&source) {
                Ok(removed) => previous_source_session.retain_removed(removed.window),
                Err(error) => {
                    self.sessions
                        .restore_structure_checkpoint(previous_source_session)?;
                    return Err(error);
                }
            }
        }
        self.options.move_window_overrides(&source, &target);
        self.hooks.move_window_hooks(&source, &target);
        self.move_window_link_slot(
            &source_session_name,
            source.window_index(),
            &target_session_name,
            target.window_index(),
        );
        self.move_auto_named_window_slot(
            &source_session_name,
            source.window_index(),
            &target_session_name,
            target.window_index(),
        );
        let source_runtime_after =
            self.runtime_session_name_for_window(&target_session_name, target.window_index());
        let detached_target_runtime_transfer =
            if let Some(survivor_slot) = target_link_runtime_transfer_slot.as_ref() {
                match self.transfer_detached_window_link_runtime(
                    &target_runtime_before,
                    survivor_slot,
                    &replaced_target_pane_ids,
                ) {
                    Ok(transfer) => transfer,
                    Err(error) => {
                        self.options = previous_options;
                        self.hooks = previous_hooks;
                        self.auto_named_windows = previous_auto_named_windows;
                        self.window_link_slots = previous_window_link_slots;
                        self.window_link_groups = previous_window_link_groups;
                        self.window_link_occurrences = previous_window_link_occurrences;
                        self.restore_cross_session_window_change(
                            &source_session_name,
                            previous_source_session,
                            &target_session_name,
                        )?;
                        return Err(error);
                    }
                }
            } else {
                None
            };

        if !detached {
            self.sessions.select_window(&target)?;
        }

        let removed_target_terminals = if removed_target_pane_ids.is_empty() {
            HashMap::new()
        } else {
            match self
                .terminals
                .remove_pane_batch(&target_runtime_before, &removed_target_pane_ids)
            {
                Ok(removed_target_terminals) => removed_target_terminals,
                Err(error) => {
                    self.options = previous_options;
                    self.hooks = previous_hooks;
                    self.auto_named_windows = previous_auto_named_windows;
                    self.window_link_slots = previous_window_link_slots;
                    self.window_link_groups = previous_window_link_groups;
                    self.window_link_occurrences = previous_window_link_occurrences;
                    self.restore_cross_session_window_change(
                        &source_session_name,
                        previous_source_session,
                        &target_session_name,
                    )?;
                    return Err(error);
                }
            }
        };
        let mut removed_target_outputs =
            self.remove_pane_outputs(&target_runtime_before, &removed_target_pane_ids);
        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_runtime_before,
            &source_runtime_after,
            &source_pane_ids,
        ) {
            self.rollback_detached_window_link_runtime(&detached_target_runtime_transfer)?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
            )?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_runtime_before, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_runtime_before, removed_target_outputs);
            self.resize_after_cross_session_move(
                &source_session_name,
                &target_session_name,
                source_was_last_window,
            )?;
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_runtime_before,
            &source_runtime_after,
            &source_pane_ids,
        ) {
            self.terminals.move_panes_between_sessions(
                &source_runtime_after,
                &source_runtime_before,
                &source_pane_ids,
            )?;
            self.rollback_detached_window_link_runtime(&detached_target_runtime_transfer)?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
            )?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_runtime_before, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_runtime_before, removed_target_outputs);
            self.resize_after_cross_session_move(
                &source_session_name,
                &target_session_name,
                source_was_last_window,
            )?;
            return Err(error);
        }

        if let Err(error) = self.resize_after_cross_session_move(
            &source_session_name,
            &target_session_name,
            source_was_last_window,
        ) {
            self.terminals.move_panes_between_sessions(
                &source_runtime_after,
                &source_runtime_before,
                &source_pane_ids,
            )?;
            self.move_pane_outputs_between_sessions(
                &source_runtime_after,
                &source_runtime_before,
                &source_pane_ids,
            )?;
            self.rollback_detached_window_link_runtime(&detached_target_runtime_transfer)?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_runtime_before, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_runtime_before, removed_target_outputs);
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
            )?;
            return Err(error);
        }

        if source_was_last_window {
            if source_group_members_before.len() > 1 {
                self.remove_empty_source_session_group(source_group_members_before)?;
            } else {
                let current_runtime_owner = self.sessions.runtime_owner(&source_session_name);
                let next_runtime_owner = self
                    .sessions
                    .runtime_owner_transfer_target(&source_session_name);
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
        } else {
            self.synchronize_session_group_from(&source_session_name)?;
            self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        }
        if source_session_name != target_session_name {
            if let Some(index_map) = target_group_index_map {
                self.synchronize_session_group_from_with_window_selection_map(
                    &target_session_name,
                    index_map,
                )?;
            } else {
                self.synchronize_session_group_from(&target_session_name)?;
            }
        }
        removed_target_outputs.abort_output_readers();
        self.remove_pane_lifecycles(&removed_target_pane_ids);
        if source_session_name != target_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
        }

        Ok(MoveWindowResponse {
            session_name: target_session_name.clone(),
            target: Some(WindowTarget::with_window(
                target_session_name,
                target.window_index(),
            )),
        })
    }

    fn resize_two_sessions(
        &mut self,
        source_session_name: &SessionName,
        target_session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.resize_terminals(source_session_name)?;
        self.resize_terminals(target_session_name)
    }

    fn resize_after_cross_session_move(
        &mut self,
        source_session_name: &SessionName,
        target_session_name: &SessionName,
        source_session_will_be_removed: bool,
    ) -> Result<(), RmuxError> {
        if source_session_will_be_removed {
            self.resize_terminals(target_session_name)
        } else {
            self.resize_two_sessions(source_session_name, target_session_name)
        }
    }

    fn replace_two_sessions(
        &mut self,
        source_session_name: &SessionName,
        previous_source_session: rmux_core::ManagedStructureCheckpoint,
        target_session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        let _ = (source_session_name, target_session_name);
        self.sessions
            .restore_structure_checkpoint(previous_source_session)
    }

    fn restore_cross_session_window_change(
        &mut self,
        source_session_name: &SessionName,
        previous_source_session: rmux_core::ManagedStructureCheckpoint,
        target_session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        self.replace_two_sessions(
            source_session_name,
            previous_source_session,
            target_session_name,
        )?;
        self.resize_two_sessions(source_session_name, target_session_name)
    }
}
