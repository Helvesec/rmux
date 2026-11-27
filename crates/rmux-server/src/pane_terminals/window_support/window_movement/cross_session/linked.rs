//! Moving a window that belongs to a link group into another session, and
//! restoring the link state when that move fails.

use std::collections::{BTreeMap, HashMap, HashSet};

use rmux_core::EnvironmentStore;
use rmux_proto::{LinkWindowRequest, MoveWindowResponse, RmuxError, SessionName, WindowTarget};

use crate::pane_terminals::{WindowLinkGroup, WindowLinkOccurrenceId, WindowLinkSlot};

use super::super::super::{session_not_found, HandlerState};

impl HandlerState {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn move_linked_window_across_sessions(
        &mut self,
        source: WindowTarget,
        target: WindowTarget,
        kill_destination: bool,
        detached: bool,
        target_group_index_map: Option<&BTreeMap<u32, u32>>,
        mut previous_source_session: rmux_core::ManagedStructureCheckpoint,
        previous_options: rmux_core::OptionStore,
        previous_hooks: rmux_core::HookStore,
    ) -> Result<MoveWindowResponse, RmuxError> {
        let source_session_name = source.session_name().clone();
        let target_session_name = target.session_name().clone();
        let previous_auto_named_windows = self.auto_named_windows.clone();
        let previous_window_link_slots = self.window_link_slots.clone();
        let previous_window_link_groups = self.window_link_groups.clone();
        let previous_window_link_occurrences = self.window_link_occurrences.clone();
        let previous_environment = self.environment.clone();
        let source_occurrence_id =
            self.window_link_occurrence_id(&source_session_name, source.window_index());
        let source_view = self
            .sessions
            .session(&source_session_name)
            .ok_or_else(|| session_not_found(&source_session_name))?;
        let source_was_last_window = source_view.windows().count() == 1;
        let source_alert_flags = source_view.winlink_alert_flags(source.window_index());
        let source_group_members_before = if source_was_last_window {
            self.sessions.session_group_members(&source_session_name)
        } else {
            Vec::new()
        };

        if let Err(error) = self.link_window_with_existing_target_index_map(
            LinkWindowRequest {
                source: source.clone(),
                target: target.clone(),
                after: false,
                before: false,
                kill_destination,
                // Selection happens after the moved winlink's alert flags are
                // installed, matching the atomic same-session move path.
                detached: true,
            },
            target_group_index_map,
            Some(&mut previous_source_session),
        ) {
            self.restore_linked_window_move_state(
                &source_session_name,
                previous_source_session,
                &target_session_name,
                previous_environment.clone(),
                previous_options,
                previous_hooks,
                previous_auto_named_windows,
                previous_window_link_slots,
                previous_window_link_groups,
                previous_window_link_occurrences,
            )?;
            return Err(error);
        }
        self.sessions
            .replace_winlink_alert_flags(&target, source_alert_flags)?;

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
                previous_environment,
                previous_options,
                previous_hooks,
                previous_auto_named_windows,
                previous_window_link_slots,
                previous_window_link_groups,
                previous_window_link_occurrences,
            )?;
            return Err(error);
        }

        if let Some(occurrence_id) = source_occurrence_id {
            self.set_window_link_occurrence_id(
                &target_session_name,
                target.window_index(),
                occurrence_id,
            );
        }

        if !detached {
            self.sessions.select_window(&target)?;
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
        previous_source_session: rmux_core::ManagedStructureCheckpoint,
        target_session_name: &SessionName,
        previous_environment: EnvironmentStore,
        previous_options: rmux_core::OptionStore,
        previous_hooks: rmux_core::HookStore,
        previous_auto_named_windows: HashSet<(SessionName, u32)>,
        previous_window_link_slots: HashMap<WindowLinkSlot, u64>,
        previous_window_link_groups: HashMap<u64, WindowLinkGroup>,
        previous_window_link_occurrences: HashMap<WindowLinkSlot, WindowLinkOccurrenceId>,
    ) -> Result<(), RmuxError> {
        self.replace_two_sessions(
            source_session_name,
            previous_source_session,
            target_session_name,
        )?;
        self.environment = previous_environment;
        self.options = previous_options;
        self.hooks = previous_hooks;
        self.auto_named_windows = previous_auto_named_windows;
        self.window_link_slots = previous_window_link_slots;
        self.window_link_groups = previous_window_link_groups;
        self.window_link_occurrences = previous_window_link_occurrences;
        Ok(())
    }
}
