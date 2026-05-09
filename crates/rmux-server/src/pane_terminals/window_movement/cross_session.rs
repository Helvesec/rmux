use std::collections::HashMap;

use rmux_core::Session;
use rmux_proto::{MoveWindowResponse, RmuxError, SessionName, SwapWindowResponse, WindowTarget};

use super::super::{session_not_found, window_pane_ids, HandlerState};

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

        self.ensure_panes_exist(&source_session_name, &source_pane_ids)?;
        self.ensure_panes_exist(&target_session_name, &target_pane_ids)?;

        self.sessions
            .session_mut(&source_session_name)
            .ok_or_else(|| session_not_found(&source_session_name))?
            .replace_window(source.window_index(), target_window)?;
        self.sessions
            .session_mut(&target_session_name)
            .ok_or_else(|| session_not_found(&target_session_name))?
            .replace_window(target.window_index(), source_window)?;
        self.swap_auto_named_window_slots(
            &source_session_name,
            source.window_index(),
            &target_session_name,
            target.window_index(),
        );
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

        if let Err(error) = self.terminals.swap_panes_between_sessions(
            &source_session_name,
            &source_pane_ids,
            &target_session_name,
            &target_pane_ids,
        ) {
            self.swap_auto_named_window_slots(
                &source_session_name,
                source.window_index(),
                &target_session_name,
                target.window_index(),
            );
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }
        if let Err(error) = self.swap_pane_outputs_between_sessions(
            &source_session_name,
            &source_pane_ids,
            &target_session_name,
            &target_pane_ids,
        ) {
            self.terminals.swap_panes_between_sessions(
                &source_session_name,
                &target_pane_ids,
                &target_session_name,
                &source_pane_ids,
            )?;
            self.swap_auto_named_window_slots(
                &source_session_name,
                source.window_index(),
                &target_session_name,
                target.window_index(),
            );
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            return Err(error);
        }

        if let Err(error) = self.resize_two_sessions(&source_session_name, &target_session_name) {
            self.terminals.swap_panes_between_sessions(
                &source_session_name,
                &target_pane_ids,
                &target_session_name,
                &source_pane_ids,
            )?;
            self.swap_pane_outputs_between_sessions(
                &source_session_name,
                &target_pane_ids,
                &target_session_name,
                &source_pane_ids,
            )?;
            self.swap_auto_named_window_slots(
                &source_session_name,
                source.window_index(),
                &target_session_name,
                target.window_index(),
            );
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

    pub(super) fn move_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        kill_destination: bool,
        detached: bool,
    ) -> Result<MoveWindowResponse, RmuxError> {
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
        if previous_source_session.windows().len() == 1 {
            return Err(RmuxError::Server(format!(
                "cannot kill the only window in session {}",
                source_session_name
            )));
        }
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
            return Err(RmuxError::invalid_target(
                target.to_string(),
                "window index already exists in session",
            ));
        }

        let source_pane_ids = window_pane_ids(
            &previous_source_session,
            &source_session_name,
            source.window_index(),
        )?;
        let removed_target_pane_ids = if target_exists && kill_destination {
            window_pane_ids(
                &previous_target_session,
                &target_session_name,
                target.window_index(),
            )?
        } else {
            Vec::new()
        };
        self.ensure_panes_exist(&source_session_name, &source_pane_ids)?;
        self.ensure_panes_exist(&target_session_name, &[])?;
        if !removed_target_pane_ids.is_empty() {
            self.ensure_panes_exist(&target_session_name, &removed_target_pane_ids)?;
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

        let source_removal_result = self
            .sessions
            .session_mut(&source_session_name)
            .ok_or_else(|| session_not_found(&source_session_name))?
            .remove_window(source.window_index());
        if let Err(error) = source_removal_result {
            self.replace_session(&target_session_name, previous_target_session)?;
            return Err(error);
        }

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
                .remove_pane_batch(&target_session_name, &removed_target_pane_ids)
            {
                Ok(removed_target_terminals) => removed_target_terminals,
                Err(error) => {
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
        let removed_target_outputs =
            self.remove_pane_outputs(&target_session_name, &removed_target_pane_ids);
        if let Err(error) = self.terminals.move_panes_between_sessions(
            &source_session_name,
            &target_session_name,
            &source_pane_ids,
        ) {
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_session_name, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_session_name, removed_target_outputs);
            self.resize_two_sessions(&source_session_name, &target_session_name)?;
            return Err(error);
        }
        if let Err(error) = self.move_pane_outputs_between_sessions(
            &source_session_name,
            &target_session_name,
            &source_pane_ids,
        ) {
            self.terminals.move_panes_between_sessions(
                &target_session_name,
                &source_session_name,
                &source_pane_ids,
            )?;
            self.replace_two_sessions(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_target_session,
            )?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_session_name, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_session_name, removed_target_outputs);
            self.resize_two_sessions(&source_session_name, &target_session_name)?;
            return Err(error);
        }

        if let Err(error) = self.resize_two_sessions(&source_session_name, &target_session_name) {
            self.terminals.move_panes_between_sessions(
                &target_session_name,
                &source_session_name,
                &source_pane_ids,
            )?;
            self.move_pane_outputs_between_sessions(
                &target_session_name,
                &source_session_name,
                &source_pane_ids,
            )?;
            if !removed_target_terminals.is_empty() {
                self.terminals
                    .insert_existing_panes(&target_session_name, removed_target_terminals)?;
            }
            self.insert_existing_pane_outputs(&target_session_name, removed_target_outputs);
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
        self.remove_pane_lifecycles(&removed_target_pane_ids);
        self.sync_pane_lifecycle_dimensions_for_session(&source_session_name);
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
