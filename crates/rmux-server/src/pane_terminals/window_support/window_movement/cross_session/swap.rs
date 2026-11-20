//! Swapping two windows that live in different sessions, with rollback of
//! the moved pane terminals and outputs on failure.

use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName, SwapWindowResponse, WindowTarget};

use super::super::super::{session_not_found, window_pane_ids, HandlerState};

impl HandlerState {
    pub(in crate::pane_terminals::window_support::window_movement) fn swap_window_across_sessions(
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
        let source_alert_flags = previous_source_session.winlink_alert_flags(source.window_index());
        let target_alert_flags = previous_target_session.winlink_alert_flags(target.window_index());
        let source_pane_ids = window_pane_ids(
            previous_source_session,
            &source_session_name,
            source.window_index(),
        )?;
        let target_pane_ids = window_pane_ids(
            previous_target_session,
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

        let previous_source_session = self
            .sessions
            .checkpoint_structure(&[source_session_name.clone(), target_session_name.clone()])?;
        self.sessions
            .swap_window_occurrences_with_selection(&source, &target, detached)?;
        self.sessions
            .replace_winlink_alert_flags(&source, target_alert_flags)?;
        self.sessions
            .replace_winlink_alert_flags(&target, source_alert_flags)?;
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
            self.sessions.select_window(&source)?;
            self.sessions.select_window(&target)?;
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
            self.window_link_occurrences = previous_window_link_occurrences;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
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
            self.window_link_occurrences = previous_window_link_occurrences;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
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
            self.window_link_occurrences = previous_window_link_occurrences;
            self.restore_cross_session_window_change(
                &source_session_name,
                previous_source_session,
                &target_session_name,
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
}
