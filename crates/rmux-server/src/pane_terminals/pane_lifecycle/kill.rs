use std::collections::HashSet;

use rmux_proto::{KillPaneResponse, PaneTarget, RmuxError, WindowTarget};

use super::super::{
    session_not_found, HandlerState, KilledPaneHookContext, KilledPaneResult,
    SessionTransferSnapshot,
};
use super::preview::preview_kill_pane;
use super::terminate_removed_terminals;

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupedLastPaneAction {
    KillSharedPane,
    RemoveAddressedAlias,
}

impl HandlerState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kill_pane(&mut self, target: PaneTarget) -> Result<KilledPaneResult, RmuxError> {
        self.kill_pane_with_options(target, false)
    }

    pub(crate) fn kill_pane_with_options(
        &mut self,
        target: PaneTarget,
        kill_all_except: bool,
    ) -> Result<KilledPaneResult, RmuxError> {
        self.kill_pane_with_grouped_last_pane_action(
            target,
            kill_all_except,
            GroupedLastPaneAction::KillSharedPane,
        )
    }

    pub(crate) fn remove_pane_alias_with_options(
        &mut self,
        target: PaneTarget,
        kill_all_except: bool,
    ) -> Result<KilledPaneResult, RmuxError> {
        self.kill_pane_with_grouped_last_pane_action(
            target,
            kill_all_except,
            GroupedLastPaneAction::RemoveAddressedAlias,
        )
    }

    fn kill_pane_with_grouped_last_pane_action(
        &mut self,
        target: PaneTarget,
        kill_all_except: bool,
        grouped_last_pane_action: GroupedLastPaneAction,
    ) -> Result<KilledPaneResult, RmuxError> {
        if kill_all_except {
            // The kept pane is resized over the window: a poisoned one fails
            // fast before any other pane is killed.
            self.ensure_pane_available(&target)?;
        }
        let session_name = target.session_name().clone();
        let previous_session = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let (hook_context, pane_id, addressed_last_pane, remove_session) = {
            let window = previous_session
                .window_at(target.window_index())
                .ok_or_else(|| {
                    RmuxError::invalid_target(
                        format!("{}:{}", target.session_name(), target.window_index()),
                        "window index does not exist in session",
                    )
                })?;
            let pane = window.pane(target.pane_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                )
            })?;
            let pane_id = pane.id();
            let hook_context = KilledPaneHookContext {
                target: target.clone(),
                pane_id: pane_id.as_u32(),
                window_id: window.id().as_u32(),
                window_name: window.name().unwrap_or_default().to_owned(),
            };
            let addressed_last_pane = !kill_all_except && window.pane_count() == 1;
            (
                hook_context,
                pane_id,
                addressed_last_pane,
                addressed_last_pane && previous_session.windows().count() == 1,
            )
        };
        let linked_window_family = self.window_link_count(&session_name, target.window_index()) > 1;
        let grouped_session_family =
            self.window_linked_session_count(&session_name, target.window_index()) > 1;
        let remove_complete_family = grouped_last_pane_action
            == GroupedLastPaneAction::KillSharedPane
            && (linked_window_family || grouped_session_family);
        if addressed_last_pane && remove_complete_family {
            return self.kill_last_linked_pane(target, hook_context, pane_id);
        }
        if remove_session {
            let mut affected_sessions =
                self.window_linked_session_family_list(&session_name, target.window_index());
            if affected_sessions.is_empty() {
                affected_sessions.push(session_name.clone());
            }
            affected_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            affected_sessions.dedup();
            self.ensure_panes_exist(&session_name, &[pane_id])?;
            let current_runtime_owner = self.sessions.runtime_owner(&session_name);
            let next_runtime_owner = self.sessions.runtime_owner_transfer_target(&session_name);
            let removed_session = self.sessions.remove_session(&session_name)?;
            self.clear_marked_pane_if_id(pane_id);
            let _ = self.options.remove_session(&session_name);
            let _ = self.environment.remove_session(&session_name);
            self.remove_session_terminals(
                &session_name,
                current_runtime_owner.as_ref(),
                next_runtime_owner.as_ref(),
            )?;
            let removed_pane_ids = self.pane_ids_no_longer_referenced([pane_id]);
            return Ok(KilledPaneResult {
                response: KillPaneResponse {
                    target,
                    window_destroyed: true,
                },
                hook_context,
                session_destroyed: true,
                removed_session_id: Some(removed_session.id().as_u32()),
                removed_pane_ids,
                affected_sessions,
                destroyed_sessions: vec![(session_name, removed_session.id().as_u32())],
                reindexed_windows: Vec::new(),
            });
        }
        if addressed_last_pane
            && grouped_last_pane_action == GroupedLastPaneAction::RemoveAddressedAlias
            && linked_window_family
        {
            return self.remove_last_pane_addressed_window_alias(target, hook_context);
        }

        let window_index = target.window_index();
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let linked_slots = self.window_link_slots_for(&session_name, window_index);
        let before_pane_options = if linked_slots.len() > 1 {
            self.window_linked_session_family_list(&session_name, window_index)
                .into_iter()
                .map(|linked_session| {
                    let snapshot = if linked_session == session_name {
                        before_pane_options.clone()
                    } else {
                        self.pane_option_slots_for_session(&linked_session)?
                    };
                    Ok((linked_session, snapshot))
                })
                .collect::<Result<Vec<_>, RmuxError>>()?
        } else {
            vec![(session_name.clone(), before_pane_options)]
        };
        let preview_outcome = preview_kill_pane(&self.sessions, &target, kill_all_except)?;
        self.ensure_window_panes_exist(
            &session_name,
            window_index,
            preview_outcome.removed_pane_ids(),
        )?;
        let mut transfer_snapshot =
            SessionTransferSnapshot::capture(self, std::slice::from_ref(&session_name))?;
        let window_target = WindowTarget::with_window(session_name.clone(), target.window_index());
        let removal = if kill_all_except {
            self.sessions
                .kill_other_panes(&window_target, target.pane_index())?
        } else {
            self.sessions
                .kill_pane(&window_target, target.pane_index())?
        };
        debug_assert_eq!(removal.pane_ids, preview_outcome.removed_pane_ids());
        debug_assert_eq!(
            removal.destroyed_window.is_some(),
            preview_outcome.window_destroyed()
        );
        if let Some(removed) = removal.destroyed_window {
            transfer_snapshot.retain_removed(removed.window);
        }
        let committed_outcome = preview_outcome;
        let removed_pane_ids = committed_outcome.removed_pane_ids().to_vec();
        if committed_outcome.window_destroyed() {
            let destroyed_window =
                WindowTarget::with_window(session_name.clone(), target.window_index());
            let _ = self.options.remove_window(&destroyed_window);
            let _ = self.hooks.remove_window(&destroyed_window);
            self.clear_auto_named_window(&session_name, target.window_index());
            let _ = self.detach_window_link_slot(&session_name, target.window_index());
        }
        let mut manual_size_refitted = false;
        let mut affected_sessions = if committed_outcome.window_destroyed() {
            vec![session_name.clone()]
        } else {
            let synchronize_result = (|| {
                // The layout lost panes, so its minimum may have fallen below
                // the size a manual window was held at.
                manual_size_refitted = self.refit_manual_window_size(&window_target)?;
                self.synchronize_linked_window_from_slot(&session_name, window_index)?;
                let mut synchronized_sessions = HashSet::new();
                for slot in &linked_slots {
                    if synchronized_sessions.insert(slot.session_name.clone()) {
                        self.synchronize_session_group_from(&slot.session_name)?;
                    }
                }
                Ok::<_, RmuxError>(
                    self.window_linked_session_family_list(&session_name, window_index),
                )
            })();
            match synchronize_result {
                Ok(sessions) => sessions,
                Err(error) => {
                    transfer_snapshot.restore(self)?;
                    return Err(error);
                }
            }
        };
        let reindexed_windows = match (|| {
            for (affected_session, before) in before_pane_options {
                self.rekey_pane_options_after_session_change(&before, &affected_session)?;
            }
            if !committed_outcome.window_destroyed() {
                return Ok(Default::default());
            }
            self.renumber_windows_if_enabled(&session_name)
        })() {
            Ok(index_map) if !index_map.is_empty() => vec![(session_name.clone(), index_map)],
            Ok(_) => Vec::new(),
            Err(error) => {
                transfer_snapshot.restore(self)?;
                return Err(error);
            }
        };

        #[cfg(windows)]
        let terminal_pane_ids = committed_outcome
            .removed_pane_ids()
            .iter()
            .copied()
            .filter(|pane_id| {
                self.terminals
                    .ensure_panes_exist(&runtime_session_name, &[*pane_id])
                    .is_ok()
            })
            .collect::<Vec<_>>();
        #[cfg(not(windows))]
        let terminal_pane_ids = committed_outcome.removed_pane_ids().to_vec();
        let mut removed_terminals = if terminal_pane_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            match self
                .terminals
                .remove_pane_batch(&runtime_session_name, &terminal_pane_ids)
            {
                Ok(removed_terminals) => removed_terminals,
                Err(error) => {
                    transfer_snapshot.restore(self)?;
                    return Err(error);
                }
            }
        };
        let mut removed_outputs =
            self.remove_pane_outputs(&runtime_session_name, committed_outcome.removed_pane_ids());

        if let Err(error) = self.resize_terminals(&session_name) {
            let terminal_rollback = self
                .terminals
                .insert_existing_panes(&runtime_session_name, removed_terminals);
            self.insert_existing_pane_outputs(&runtime_session_name, removed_outputs);
            transfer_snapshot.restore(self)?;
            let resize_rollback = self.resize_terminals(&session_name);
            terminal_rollback.map_err(|rollback_error| {
                RmuxError::Server(format!(
                    "failed to restore pane terminals for runtime session {runtime_session_name} after {error}: {rollback_error}"
                ))
            })?;
            resize_rollback.map_err(|rollback_error| {
                RmuxError::Server(format!(
                    "failed to roll back session {session_name} after {error}: {rollback_error}"
                ))
            })?;
            return Err(error);
        }
        if manual_size_refitted {
            // tmux's resize_window() fires window-resized for the refit.
            self.record_applied_window_resize(window_target.clone());
        }
        for pane_id in committed_outcome.removed_pane_ids() {
            self.clear_marked_pane_if_id(*pane_id);
        }
        #[cfg(windows)]
        for pane_id in committed_outcome.removed_pane_ids() {
            let _ = self.cancel_starting_pane(&runtime_session_name, *pane_id);
        }
        removed_outputs.abort_output_readers();
        terminate_removed_terminals(&mut removed_terminals);
        self.remove_pane_lifecycles(committed_outcome.removed_pane_ids());

        if committed_outcome.window_destroyed() {
            for synchronized_session in self.synchronize_session_group_from(&session_name)? {
                if !affected_sessions.contains(&synchronized_session) {
                    affected_sessions.push(synchronized_session);
                }
            }
        }
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        let removed_pane_ids = self.pane_ids_no_longer_referenced(removed_pane_ids);
        Ok(KilledPaneResult {
            response: KillPaneResponse {
                target,
                window_destroyed: committed_outcome.window_destroyed(),
            },
            hook_context,
            session_destroyed: false,
            removed_session_id: None,
            removed_pane_ids,
            affected_sessions,
            destroyed_sessions: Vec::new(),
            reindexed_windows,
        })
    }

    fn remove_last_pane_addressed_window_alias(
        &mut self,
        target: PaneTarget,
        hook_context: KilledPaneHookContext,
    ) -> Result<KilledPaneResult, RmuxError> {
        let session_name = target.session_name().clone();
        let mut affected_sessions =
            self.window_linked_session_family_list(&session_name, target.window_index());
        affected_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        affected_sessions.dedup();
        let result = self.unlink_window(
            rmux_proto::WindowTarget::with_window(session_name.clone(), target.window_index()),
            false,
        )?;
        debug_assert!(
            result.removed_pane_ids.is_empty(),
            "removing one surviving window alias must preserve its shared pane runtime"
        );
        Ok(KilledPaneResult {
            response: KillPaneResponse {
                target,
                window_destroyed: true,
            },
            hook_context,
            session_destroyed: false,
            removed_session_id: None,
            removed_pane_ids: result.removed_pane_ids,
            affected_sessions,
            destroyed_sessions: Vec::new(),
            reindexed_windows: Vec::new(),
        })
    }
}
