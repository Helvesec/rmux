use std::collections::{BTreeMap, HashMap};

use rmux_proto::{
    MoveWindowRequest, MoveWindowResponse, MoveWindowTarget, RmuxError, RotateWindowDirection,
    RotateWindowResponse, SessionName, SwapWindowResponse, WindowTarget,
};

use super::{
    ensure_session_panes_exist, link_window_destination_index, request_target_string,
    session_not_found, window_pane_ids, HandlerState, RemovedWindowHookContext,
};
use crate::pane_terminals::MovedWindowResult;

#[path = "window_movement/cross_session.rs"]
mod cross_session;
#[path = "window_movement/relative.rs"]
mod relative;

impl HandlerState {
    fn reject_window_move_between_grouped_sessions(
        &self,
        source_session_name: &SessionName,
        target_session_name: &SessionName,
    ) -> Result<(), RmuxError> {
        if source_session_name == target_session_name {
            return Ok(());
        }

        let source_group = self.sessions.session_group_name(source_session_name);
        let target_group = self.sessions.session_group_name(target_session_name);
        if matches!((source_group, target_group), (Some(left), Some(right)) if left == right) {
            return Err(RmuxError::Server(
                "can't move window, sessions are grouped".to_owned(),
            ));
        }

        Ok(())
    }

    pub(crate) fn move_window(
        &mut self,
        request: MoveWindowRequest,
    ) -> Result<MovedWindowResult, RmuxError> {
        let unlinked_window = move_window_unlinked_context(self, &request);
        let replaced_pane_ids = move_window_replaced_pane_ids(self, &request);
        let response = self.move_window_response(request)?;
        Ok(MovedWindowResult {
            response,
            unlinked_window,
            removed_pane_ids: self.pane_ids_no_longer_referenced(replaced_pane_ids),
        })
    }

    fn move_window_response(
        &mut self,
        request: MoveWindowRequest,
    ) -> Result<MoveWindowResponse, RmuxError> {
        if request.after || request.before {
            return self.move_window_relative(request);
        }
        if request.renumber {
            return match request.target {
                MoveWindowTarget::Session(target_session_name) => {
                    self.reindex_windows(MoveWindowRequest {
                        source: None,
                        target: MoveWindowTarget::Session(target_session_name),
                        renumber: true,
                        kill_destination: false,
                        detached: request.detached,
                        after: false,
                        before: false,
                    })
                }
                MoveWindowTarget::Window(target) => {
                    let target_session_name = target.session_name().clone();
                    self.reindex_windows(MoveWindowRequest {
                        source: None,
                        target: MoveWindowTarget::Session(target_session_name),
                        renumber: true,
                        kill_destination: false,
                        detached: request.detached,
                        after: false,
                        before: false,
                    })
                }
            };
        }

        let source = request
            .source
            .ok_or_else(|| RmuxError::Server("move-window requires a source window".to_owned()))?;
        let target = match request.target {
            MoveWindowTarget::Window(target) => target,
            MoveWindowTarget::Session(session_name) => {
                let destination_index = self.first_available_window_index(&session_name)?;
                WindowTarget::with_window(session_name, destination_index)
            }
        };
        if source.session_name() == target.session_name()
            && source.window_index() == target.window_index()
        {
            if !request.kill_destination {
                return Ok(MoveWindowResponse {
                    session_name: source.session_name().clone(),
                    target: Some(source),
                });
            }
            return Err(RmuxError::Server(format!(
                "same index: {}",
                target.window_index()
            )));
        }

        if source.session_name() == target.session_name() {
            let winlink_alert_map =
                BTreeMap::from([(source.window_index(), target.window_index())]);
            return self.move_window_within_session(
                source,
                target.window_index(),
                request.kill_destination,
                request.detached,
                &winlink_alert_map,
            );
        }

        self.move_window_across_sessions(
            source,
            target,
            request.kill_destination,
            request.detached,
            None,
        )
    }

    pub(in crate::pane_terminals) fn first_available_window_index(
        &self,
        session_name: &SessionName,
    ) -> Result<u32, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let base_index = self.session_base_index(session_name);
        let mut index = base_index;
        loop {
            if session.window_at(index).is_none() {
                return Ok(index);
            }
            index = index.checked_add(1).ok_or_else(|| {
                RmuxError::Server(format!(
                    "window index space exhausted for session {session_name}"
                ))
            })?;
        }
    }

    pub(crate) fn swap_window(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        detached: bool,
    ) -> Result<SwapWindowResponse, RmuxError> {
        // tmux cmd-swap-window.c:59-65: reject swaps between different sessions
        // in the same session group.
        self.reject_window_move_between_grouped_sessions(
            source.session_name(),
            target.session_name(),
        )?;

        if source.session_name() == target.session_name() {
            let session_name = source.session_name().clone();
            let previous_session = self
                .sessions
                .session(&session_name)
                .cloned()
                .ok_or_else(|| session_not_found(&session_name))?;
            let previous_options = self.options.clone();
            let previous_hooks = self.hooks.clone();
            let previous_auto_named_windows = self.auto_named_windows.clone();
            let previous_window_link_slots = self.window_link_slots.clone();
            let previous_window_link_groups = self.window_link_groups.clone();
            let previous_window_link_occurrences = self.window_link_occurrences.clone();
            ensure_session_panes_exist(self, &session_name, &previous_session)?;

            {
                let session = self
                    .sessions
                    .session_mut(&session_name)
                    .ok_or_else(|| session_not_found(&session_name))?;
                session.swap_windows(source.window_index(), target.window_index())?;
                // tmux preserves the current winlink unless -d is passed. With
                // -d, it selects the destination winlink after swapping.
                if detached {
                    session.select_window(target.window_index())?;
                }
            }
            let source_slot =
                WindowTarget::with_window(session_name.clone(), source.window_index());
            let target_slot =
                WindowTarget::with_window(session_name.clone(), target.window_index());
            self.options
                .swap_window_overrides(&source_slot, &target_slot);
            self.hooks.swap_window_hooks(&source_slot, &target_slot);
            self.swap_window_link_slots(
                &session_name,
                source.window_index(),
                target.window_index(),
            );
            self.swap_auto_named_window_slots(
                &session_name,
                source.window_index(),
                &session_name,
                target.window_index(),
            );

            if let Err(error) = self.resize_terminals(&session_name) {
                self.options = previous_options;
                self.hooks = previous_hooks;
                self.auto_named_windows = previous_auto_named_windows;
                self.window_link_slots = previous_window_link_slots;
                self.window_link_groups = previous_window_link_groups;
                self.window_link_occurrences = previous_window_link_occurrences;
                self.restore_session_after_resize_error(&session_name, previous_session, &error)?;
                return Err(error);
            }
            let window_selection_map = BTreeMap::new();
            let winlink_alert_map = BTreeMap::from([
                (source.window_index(), target.window_index()),
                (target.window_index(), source.window_index()),
            ]);
            self.synchronize_session_group_from_with_window_selection_and_winlink_alert_maps(
                &session_name,
                &window_selection_map,
                &winlink_alert_map,
            )?;

            return Ok(SwapWindowResponse { source, target });
        }

        self.swap_window_across_sessions(source, target, detached)
    }

    pub(crate) fn rotate_window(
        &mut self,
        target: WindowTarget,
        direction: RotateWindowDirection,
        restore_zoom: bool,
    ) -> Result<RotateWindowResponse, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();

        self.mutate_session_and_resize_window_terminal(&session_name, window_index, |session| {
            if restore_zoom {
                session.rotate_window_with_zoom(window_index, direction, true)?;
            } else {
                session.rotate_window(window_index, direction)?;
            }
            Ok(RotateWindowResponse { target })
        })
    }

    fn reindex_windows(
        &mut self,
        request: MoveWindowRequest,
    ) -> Result<MoveWindowResponse, RmuxError> {
        if request.source.is_some() {
            return Err(RmuxError::Server(
                "move-window -r does not accept a source window".to_owned(),
            ));
        }

        let MoveWindowTarget::Session(session_name) = request.target else {
            return Err(RmuxError::invalid_target(
                request_target_string(&request.target),
                "move-window -r requires a session target",
            ));
        };

        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_window_link_occurrences = self.window_link_occurrences.clone();

        let winlink_alert_map = self.reindex_windows_from_base(&session_name)?;
        if let Err(error) = self.resize_terminals(&session_name) {
            self.replace_session(&session_name, previous_session)?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            return Err(error);
        }

        self.synchronize_session_group_from_with_winlink_alert_map(
            &session_name,
            &winlink_alert_map,
        )?;
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(MoveWindowResponse {
            session_name,
            target: None,
        })
    }

    fn move_window_within_session(
        &mut self,
        source: WindowTarget,
        destination_index: u32,
        kill_destination: bool,
        detached: bool,
        group_winlink_alert_map: &BTreeMap<u32, u32>,
    ) -> Result<MoveWindowResponse, RmuxError> {
        let session_name = source.session_name().clone();
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let previous_options = self.options.clone();
        let previous_hooks = self.hooks.clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_window_link_occurrences = self.window_link_occurrences.clone();
        ensure_session_panes_exist(self, &session_name, &previous_session)?;
        let target_link_runtime_transfer_slot = if kill_destination
            && source.window_index() != destination_index
            && self.window_link_count(&session_name, destination_index) > 1
        {
            self.linked_runtime_transfer_slot_for_detached_window(&session_name, destination_index)
        } else {
            None
        };
        let replaced_pane_ids = if kill_destination && source.window_index() != destination_index {
            previous_session
                .window_at(destination_index)
                .map(|_| window_pane_ids(&previous_session, &session_name, destination_index))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let target_runtime_before = if target_link_runtime_transfer_slot.is_some() {
            Some(self.runtime_session_name_for_window(&session_name, destination_index))
        } else {
            None
        };
        let removed_pane_ids = if target_link_runtime_transfer_slot.is_some() {
            Vec::new()
        } else {
            replaced_pane_ids.clone()
        };
        let should_select_destination = !detached
            || (kill_destination && previous_session.active_window_index() == destination_index);
        if !kill_destination && previous_session.window_at(destination_index).is_some() {
            return Err(RmuxError::Server(format!(
                "index in use: {destination_index}"
            )));
        }

        {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            session.move_window(
                source.window_index(),
                destination_index,
                kill_destination,
                should_select_destination,
            )?;
        }
        let source_slot = WindowTarget::with_window(session_name.clone(), source.window_index());
        let target_slot = WindowTarget::with_window(session_name.clone(), destination_index);
        self.options
            .move_window_overrides(&source_slot, &target_slot);
        self.hooks.move_window_hooks(&source_slot, &target_slot);
        self.move_window_link_slot(
            &session_name,
            source.window_index(),
            &session_name,
            destination_index,
        );
        self.move_auto_named_window_slot(
            &session_name,
            source.window_index(),
            &session_name,
            destination_index,
        );
        let detached_target_runtime_transfer = if let (Some(source_runtime), Some(survivor_slot)) = (
            target_runtime_before.as_ref(),
            target_link_runtime_transfer_slot.as_ref(),
        ) {
            match self.transfer_detached_window_link_runtime(
                source_runtime,
                survivor_slot,
                &replaced_pane_ids,
            ) {
                Ok(transfer) => transfer,
                Err(error) => {
                    self.options = previous_options;
                    self.hooks = previous_hooks;
                    self.auto_named_windows = previous_auto_named_windows;
                    self.window_link_slots = previous_window_link_slots;
                    self.window_link_groups = previous_window_link_groups;
                    self.window_link_occurrences = previous_window_link_occurrences;
                    self.replace_session(&session_name, previous_session)?;
                    return Err(error);
                }
            }
        } else {
            None
        };

        let removed_terminals = if removed_pane_ids.is_empty() {
            HashMap::new()
        } else {
            match self
                .terminals
                .remove_pane_batch(&session_name, &removed_pane_ids)
            {
                Ok(removed_terminals) => removed_terminals,
                Err(error) => {
                    self.options = previous_options;
                    self.hooks = previous_hooks;
                    self.auto_named_windows = previous_auto_named_windows;
                    self.window_link_slots = previous_window_link_slots;
                    self.window_link_groups = previous_window_link_groups;
                    self.window_link_occurrences = previous_window_link_occurrences;
                    self.restore_session_after_resize_error(
                        &session_name,
                        previous_session.clone(),
                        &error,
                    )?;
                    return Err(error);
                }
            }
        };
        let mut removed_outputs = self.remove_pane_outputs(&session_name, &removed_pane_ids);

        if let Err(error) = self.resize_terminals(&session_name) {
            self.rollback_detached_window_link_runtime(&detached_target_runtime_transfer)?;
            self.options = previous_options;
            self.hooks = previous_hooks;
            self.auto_named_windows = previous_auto_named_windows;
            self.window_link_slots = previous_window_link_slots;
            self.window_link_groups = previous_window_link_groups;
            self.window_link_occurrences = previous_window_link_occurrences;
            self.replace_session(&session_name, previous_session)?;
            if !removed_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&session_name, removed_terminals)?;
            }
            self.insert_existing_pane_outputs(&session_name, removed_outputs);
            self.resize_terminals(&session_name)
                .map_err(|rollback_error| {
                    RmuxError::Server(format!(
                    "failed to roll back session {session_name} after {error}: {rollback_error}"
                ))
                })?;
            return Err(error);
        }
        removed_outputs.abort_output_readers();
        self.synchronize_session_group_from_with_winlink_alert_map(
            &session_name,
            group_winlink_alert_map,
        )?;
        self.remove_pane_lifecycles(&removed_pane_ids);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(MoveWindowResponse {
            session_name: session_name.clone(),
            target: Some(WindowTarget::with_window(session_name, destination_index)),
        })
    }
}

fn move_window_unlinked_context(
    state: &HandlerState,
    request: &MoveWindowRequest,
) -> Option<RemovedWindowHookContext> {
    let source = request.source.as_ref()?;
    let window = state
        .sessions
        .session(source.session_name())?
        .window_at(source.window_index())?;
    Some(RemovedWindowHookContext {
        target: source.clone(),
        window_id: window.id().as_u32(),
        window_name: window.name().unwrap_or_default().to_owned(),
    })
}

fn move_window_replaced_pane_ids(
    state: &HandlerState,
    request: &MoveWindowRequest,
) -> Vec<rmux_core::PaneId> {
    if request.renumber || !request.kill_destination || request.after || request.before {
        return Vec::new();
    }
    let Some(source) = request.source.as_ref() else {
        return Vec::new();
    };
    let MoveWindowTarget::Window(target) = &request.target else {
        return Vec::new();
    };
    if source == target {
        return Vec::new();
    }
    state
        .sessions
        .session(target.session_name())
        .and_then(|session| session.window_at(target.window_index()))
        .map(|window| window.panes().iter().map(|pane| pane.id()).collect())
        .unwrap_or_default()
}
