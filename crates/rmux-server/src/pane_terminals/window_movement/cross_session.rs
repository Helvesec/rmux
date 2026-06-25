use std::collections::{HashMap, HashSet};

use rmux_core::{EnvironmentStore, PaneId, Session};
use rmux_proto::{
    LinkWindowRequest, MoveWindowResponse, RmuxError, SessionName, SwapWindowResponse, WindowTarget,
};

use crate::pane_terminals::{WindowLinkGroup, WindowLinkSlot};

use super::super::{session_not_found, window_pane_ids, HandlerState};

pub(super) struct DetachedWindowLinkRuntimeTransfer {
    source_runtime: SessionName,
    destination_runtime: SessionName,
    pane_ids: Vec<PaneId>,
}

impl HandlerState {
    pub(super) fn swap_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        detached: bool,
    ) -> Result<SwapWindowResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        let previous_source_session = self
            .sessions
            .session(&source_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let source_window = previous_source_session
            .window_at(source.window_index())
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    source.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let target_window = previous_target_session
            .window_at(target.window_index())
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "window index does not exist in session",
                )
            })?;
        let source_pane_ids = window_pane_ids(
            &previous_source_session,
            &source_session_name,
            source.window_index(),
        )?;
        let target_pane_ids = window_pane_ids(
            &previous_target_session,
            &target_session_name,
            target.window_index(),
        )?;
        let source_runtime_before =
            self.runtime_session_name_for_window(&source_session_name, source.window_index());
        let target_runtime_before =
            self.runtime_session_name_for_window(&target_session_name, target.window_index());

        self.terminals
            .ensure_panes_exist(&source_runtime_before, &source_pane_ids)?;
        self.terminals
            .ensure_panes_exist(&target_runtime_before, &target_pane_ids)?;

        self.sessions
            .session_mut(&source_session_name)
            .ok_or_else(|| session_not_found(&source_session_name))?
            .replace_window(source.window_index(), target_window)?;
        self.sessions
            .session_mut(&target_session_name)
            .ok_or_else(|| session_not_found(&target_session_name))?
            .replace_window(target.window_index(), source_window)?;
        self.options.swap_window_overrides(&source, &target);
        self.hooks.swap_window_hooks(&source, &target);
        self.swap_window_link_slots_between(
            &source_session_name,
            source.window_index(),
            &target_session_name,
            target.window_index(),
        );
        self.swap_auto_named_window_slots(
            &source_session_name,
            source.window_index(),
            &target_session_name,
            target.window_index(),
        );
        let source_runtime_after =
            self.runtime_session_name_for_window(&target_session_name, target.window_index());
        let target_runtime_after =
            self.runtime_session_name_for_window(&source_session_name, source.window_index());
        // tmux preserves current winlinks unless -d is passed. With -d, it
        // selects the swapped source/target winlinks in their sessions.
        if detached {
            self.sessions
                .session_mut(&source_session_name)
                .ok_or_else(|| session_not_found(&source_session_name))?
                .select_window(source.window_index())?;
            self.sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?
                .select_window(target.window_index())?;
        }

        if let Err(error) = self.move_swapped_window_terminals(
            &source_runtime_before,
            &source_runtime_after,
            &source_pane_ids,
            &target_runtime_before,
            &target_runtime_after,
            &target_pane_ids,
        ) {
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }
        if let Err(error) = self.move_swapped_window_outputs(
            &source_runtime_before,
            &source_runtime_after,
            &source_pane_ids,
            &target_runtime_before,
            &target_runtime_after,
            &target_pane_ids,
        ) {
            self.rollback_swapped_window_terminals(
                &source_runtime_before,
                &source_runtime_after,
                &source_pane_ids,
                &target_runtime_before,
                &target_runtime_after,
                &target_pane_ids,
            )?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = self.resize_two_sessions(&source_session_name, &target_session_name) {
            self.rollback_swapped_window_terminals(
                &source_runtime_before,
                &source_runtime_after,
                &source_pane_ids,
                &target_runtime_before,
                &target_runtime_after,
                &target_pane_ids,
            )?;
            self.rollback_swapped_window_outputs(
                &source_runtime_before,
                &source_runtime_after,
                &source_pane_ids,
                &target_runtime_before,
                &target_runtime_after,
                &target_pane_ids,
            )?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }
        self.synchronize_session_group_from(&source_session_name)?;
        if source_session_name != target_session_name {
            self.synchronize_session_group_from(&target_session_name)?;
        }
        self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        if source_session_name != target_session_name {
            self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);
        }

        Ok(SwapWindowResponse { source, target })
    }

    fn move_swapped_window_terminals(
        &mut self,
        source_runtime_before: &SessionName,
        source_runtime_after: &SessionName,
        source_pane_ids: &[PaneId],
        target_runtime_before: &SessionName,
        target_runtime_after: &SessionName,
        target_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        self.terminals.move_panes_between_sessions(
            source_runtime_before,
            source_runtime_after,
            source_pane_ids,
        )?;
        if let Err(error) = self.terminals.move_panes_between_sessions(
            target_runtime_before,
            target_runtime_after,
            target_pane_ids,
        ) {
            let _ = self.terminals.move_panes_between_sessions(
                source_runtime_after,
                source_runtime_before,
                source_pane_ids,
            );
            return Err(error);
        }
        Ok(())
    }

    fn rollback_swapped_window_terminals(
        &mut self,
        source_runtime_before: &SessionName,
        source_runtime_after: &SessionName,
        source_pane_ids: &[PaneId],
        target_runtime_before: &SessionName,
        target_runtime_after: &SessionName,
        target_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        self.terminals.move_panes_between_sessions(
            target_runtime_after,
            target_runtime_before,
            target_pane_ids,
        )?;
        self.terminals.move_panes_between_sessions(
            source_runtime_after,
            source_runtime_before,
            source_pane_ids,
        )
    }

    fn move_swapped_window_outputs(
        &mut self,
        source_runtime_before: &SessionName,
        source_runtime_after: &SessionName,
        source_pane_ids: &[PaneId],
        target_runtime_before: &SessionName,
        target_runtime_after: &SessionName,
        target_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        self.move_pane_outputs_between_sessions(
            source_runtime_before,
            source_runtime_after,
            source_pane_ids,
        )?;
        if let Err(error) = self.move_pane_outputs_between_sessions(
            target_runtime_before,
            target_runtime_after,
            target_pane_ids,
        ) {
            let _ = self.move_pane_outputs_between_sessions(
                source_runtime_after,
                source_runtime_before,
                source_pane_ids,
            );
            return Err(error);
        }
        Ok(())
    }

    fn rollback_swapped_window_outputs(
        &mut self,
        source_runtime_before: &SessionName,
        source_runtime_after: &SessionName,
        source_pane_ids: &[PaneId],
        target_runtime_before: &SessionName,
        target_runtime_after: &SessionName,
        target_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        self.move_pane_outputs_between_sessions(
            target_runtime_after,
            target_runtime_before,
            target_pane_ids,
        )?;
        self.move_pane_outputs_between_sessions(
            source_runtime_after,
            source_runtime_before,
            source_pane_ids,
        )
    }

    pub(super) fn move_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        kill_destination: bool,
        detached: bool,
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
            .cloned()
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let previous_target_session = self
            .sessions
            .session(&target_session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&target_session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let source_was_last_window = previous_source_session.windows().len() == 1;
        let source_group_members_before = if source_was_last_window {
            self.sessions.session_group_members(&source_session_name)
        } else {
            Vec::new()
        };
        let source_window = previous_source_session
            .window_at(source.window_index())
            .cloned()
            .ok_or_else(|| {
                RmuxError::invalid_target(
                    source.to_string(),
                    "window index does not exist in session",
                )
            })?;
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
                previous_source_session,
                previous_target_session,
                previous_options,
                previous_hooks,
            );
        }

        let source_pane_ids = window_pane_ids(
            &previous_source_session,
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
                &previous_target_session,
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

        if target_exists && kill_destination {
            self.sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?
                .replace_window(target.window_index(), source_window)?;
        } else {
            self.sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?
                .insert_existing_window(target.window_index(), source_window)?;
        }

        if !source_was_last_window {
            let source_removal_result = self
                .sessions
                .session_mut(&source_session_name)
                .ok_or_else(|| session_not_found(&source_session_name))?
                .remove_window(source.window_index());
            if let Err(error) = source_removal_result {
                self.replace_session(&target_session_name, previous_target_session)?;
                return Err(error);
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
                        self.restore_cross_session_window_change(
                            &source_session_name,
                            previous_source_session,
                            &target_session_name,
                            previous_target_session,
                        )?;
                        return Err(error);
                    }
                }
            } else {
                None
            };

        if !detached {
            self.sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?
                .select_window(target.window_index())?;
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
                    self.restore_cross_session_window_change(
                        &source_session_name,
                        previous_source_session.clone(),
                        &target_session_name,
                        previous_target_session.clone(),
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
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
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
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
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
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
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
            self.synchronize_session_group_from(&target_session_name)?;
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

    pub(super) fn transfer_detached_window_link_runtime(
        &mut self,
        source_runtime: &SessionName,
        survivor_slot: &WindowLinkSlot,
        pane_ids: &[PaneId],
    ) -> Result<Option<DetachedWindowLinkRuntimeTransfer>, RmuxError> {
        let destination_runtime = self.runtime_session_name(&survivor_slot.session_name);
        self.set_window_link_runtime_session_for_slot(survivor_slot, destination_runtime.clone());
        if source_runtime == &destination_runtime || pane_ids.is_empty() {
            return Ok(None);
        }

        self.terminals.move_panes_between_sessions(
            source_runtime,
            &destination_runtime,
            pane_ids,
        )?;
        if let Err(error) =
            self.move_pane_outputs_between_sessions(source_runtime, &destination_runtime, pane_ids)
        {
            self.terminals.move_panes_between_sessions(
                &destination_runtime,
                source_runtime,
                pane_ids,
            )?;
            return Err(error);
        }

        Ok(Some(DetachedWindowLinkRuntimeTransfer {
            source_runtime: source_runtime.clone(),
            destination_runtime,
            pane_ids: pane_ids.to_vec(),
        }))
    }

    pub(super) fn rollback_detached_window_link_runtime(
        &mut self,
        transfer: &Option<DetachedWindowLinkRuntimeTransfer>,
    ) -> Result<(), RmuxError> {
        let Some(transfer) = transfer else {
            return Ok(());
        };
        self.move_pane_outputs_between_sessions(
            &transfer.destination_runtime,
            &transfer.source_runtime,
            &transfer.pane_ids,
        )?;
        self.terminals.move_panes_between_sessions(
            &transfer.destination_runtime,
            &transfer.source_runtime,
            &transfer.pane_ids,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn move_linked_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        kill_destination: bool,
        detached: bool,
        previous_source_session: Session,
        previous_target_session: Session,
        previous_options: rmux_core::OptionStore,
        previous_hooks: rmux_core::HookStore,
    ) -> Result<MoveWindowResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_environment = self.environment.clone();
        let source_was_last_window = previous_source_session.windows().len() == 1;
        let source_group_members_before = if source_was_last_window {
            self.sessions.session_group_members(&source_session_name)
        } else {
            Vec::new()
        };

        if let Err(error) = self.link_window(LinkWindowRequest {
            source: source.clone(),
            target: target.clone(),
            after: false,
            before: false,
            kill_destination,
            detached,
        }) {
            self.restore_linked_window_move_state(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
                previous_environment.clone(),
                previous_options,
                previous_hooks,
                previous_auto_named_windows,
                previous_window_link_slots,
                previous_window_link_groups,
            )?;
            return Err(error);
        }

        self.hooks.move_window_hooks(&source, &target);

        let remove_result = if source_was_last_window {
            if source_group_members_before.len() > 1 {
                self.remove_empty_source_session_group(source_group_members_before)
            } else {
                let current_runtime_owner = self.sessions.runtime_owner(&source_session_name);
                let next_runtime_owner = self
                    .sessions
                    .runtime_owner_transfer_target(&source_session_name);
                self.sessions
                    .remove_session(&source_session_name)
                    .map(|_| ())
                    .and_then(|()| {
                        let _ = self.options.remove_session(&source_session_name);
                        let _ = self.environment.remove_session(&source_session_name);
                        let _ = self.hooks.remove_session(&source_session_name);
                        self.remove_session_terminals(
                            &source_session_name,
                            current_runtime_owner.as_ref(),
                            next_runtime_owner.as_ref(),
                        )
                        .map(|_| ())
                    })
            }
        } else {
            self.unlink_window(source.clone(), false).map(|_| ())
        };

        if let Err(error) = remove_result {
            self.restore_linked_window_move_state(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
                previous_environment,
                previous_options,
                previous_hooks,
                previous_auto_named_windows,
                previous_window_link_slots,
                previous_window_link_groups,
            )?;
            return Err(error);
        }

        if !detached {
            self.sessions
                .session_mut(&target_session_name)
                .ok_or_else(|| session_not_found(&target_session_name))?
                .select_window(target.window_index())?;
        }
        if !source_was_last_window {
            self.synchronize_session_group_from(&source_session_name)?;
            self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
        }
        self.synchronize_session_group_from(&target_session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(&target_session_name);

        Ok(MoveWindowResponse {
            session_name: target_session_name.clone(),
            target: Some(WindowTarget::with_window(
                target_session_name,
                target.window_index(),
            )),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn restore_linked_window_move_state(
        &mut self,
        source_session_name: &SessionName,
        previous_source_session: Session,
        target_session_name: &SessionName,
        previous_target_session: Session,
        previous_environment: EnvironmentStore,
        previous_options: rmux_core::OptionStore,
        previous_hooks: rmux_core::HookStore,
        previous_auto_named_windows: HashSet<(SessionName, u32)>,
        previous_window_link_slots: HashMap<WindowLinkSlot, u64>,
        previous_window_link_groups: HashMap<u64, WindowLinkGroup>,
    ) -> Result<(), RmuxError> {
        self.replace_two_sessions(
            source_session_name,
            previous_source_session,
            target_session_name,
            previous_target_session,
        )?;
        self.environment = previous_environment;
        self.options = previous_options;
        self.hooks = previous_hooks;
        self.auto_named_windows = previous_auto_named_windows;
        self.window_link_slots = previous_window_link_slots;
        self.window_link_groups = previous_window_link_groups;
        Ok(())
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
        previous_source_session: Session,
        target_session_name: &SessionName,
        previous_target_session: Session,
    ) -> Result<(), RmuxError> {
        self.replace_session(source_session_name, previous_source_session)?;
        self.replace_session(target_session_name, previous_target_session)
    }

    fn restore_cross_session_window_change(
        &mut self,
        source_session_name: &SessionName,
        previous_source_session: Session,
        target_session_name: &SessionName,
        previous_target_session: Session,
    ) -> Result<(), RmuxError> {
        self.replace_two_sessions(
            source_session_name,
            previous_source_session,
            target_session_name,
            previous_target_session,
        )?;
        self.resize_two_sessions(source_session_name, target_session_name)
    }
}
