use std::collections::{HashMap, HashSet};

use rmux_core::{PaneId, SessionStore};
use rmux_proto::{KillPaneResponse, PaneTarget, RmuxError, SessionName, WindowTarget};

use super::super::{
    session_not_found, terminate_removed_terminals, HandlerState, KilledPaneHookContext,
    KilledPaneResult, RemovedPaneOutputs, WindowLinkGroup, WindowLinkSlot,
};

#[derive(Debug)]
struct LinkedWindowRemoval {
    session_name: SessionName,
    window_index: u32,
    destroy_session: bool,
}

struct LinkedKillSnapshot {
    sessions: SessionStore,
    options: rmux_core::OptionStore,
    environment: rmux_core::EnvironmentStore,
    hooks: rmux_core::HookStore,
    auto_named_windows: HashSet<(SessionName, u32)>,
    window_link_groups: HashMap<u64, WindowLinkGroup>,
    window_link_slots: HashMap<WindowLinkSlot, u64>,
    next_window_link_group_id: u64,
}

impl LinkedKillSnapshot {
    fn capture(state: &HandlerState) -> Self {
        Self {
            sessions: state.sessions.clone(),
            options: state.options.clone(),
            environment: state.environment.clone(),
            hooks: state.hooks.clone(),
            auto_named_windows: state.auto_named_windows.clone(),
            window_link_groups: state.window_link_groups.clone(),
            window_link_slots: state.window_link_slots.clone(),
            next_window_link_group_id: state.next_window_link_group_id,
        }
    }

    fn restore(self, state: &mut HandlerState) {
        state.sessions = self.sessions;
        state.options = self.options;
        state.environment = self.environment;
        state.hooks = self.hooks;
        state.auto_named_windows = self.auto_named_windows;
        state.window_link_groups = self.window_link_groups;
        state.window_link_slots = self.window_link_slots;
        state.next_window_link_group_id = self.next_window_link_group_id;
    }
}

impl HandlerState {
    pub(super) fn kill_last_linked_pane(
        &mut self,
        target: PaneTarget,
        hook_context: KilledPaneHookContext,
        pane_id: PaneId,
    ) -> Result<KilledPaneResult, RmuxError> {
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let direct_slots = self.window_link_slots_for(&session_name, window_index);
        let removals = linked_window_removals(self, &direct_slots, pane_id)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        self.ensure_window_panes_exist(&session_name, window_index, &[pane_id])?;

        let snapshot = LinkedKillSnapshot::capture(self);
        #[cfg(windows)]
        let terminal_pane_ids = self
            .terminals
            .ensure_panes_exist(&runtime_session_name, &[pane_id])
            .is_ok()
            .then_some(vec![pane_id])
            .unwrap_or_default();
        #[cfg(not(windows))]
        let terminal_pane_ids = vec![pane_id];
        let mut removed_terminals = if terminal_pane_ids.is_empty() {
            HashMap::new()
        } else {
            self.terminals
                .remove_pane_batch(&runtime_session_name, &terminal_pane_ids)?
        };
        let mut removed_outputs = self.remove_pane_outputs(&runtime_session_name, &[pane_id]);

        let commit = self.commit_linked_window_removals(&direct_slots, &removals);
        let destroyed_sessions = match commit {
            Ok(destroyed_sessions) => destroyed_sessions,
            Err(error) => {
                snapshot.restore(self);
                restore_linked_pane_runtime(
                    self,
                    &runtime_session_name,
                    removed_terminals,
                    removed_outputs,
                    &error,
                )?;
                return Err(error);
            }
        };

        self.clear_marked_pane_if_id(pane_id);
        #[cfg(windows)]
        let _ = self.cancel_starting_pane(&runtime_session_name, pane_id);
        if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, pane_id) {
            pipe.stop();
        }
        removed_outputs.abort_output_readers();
        terminate_removed_terminals(&mut removed_terminals);
        self.remove_pane_lifecycle(pane_id);
        for (destroyed_session, _) in &destroyed_sessions {
            self.remove_destroyed_linked_session_runtime(destroyed_session);
        }

        let affected_sessions = removals
            .iter()
            .map(|removal| removal.session_name.clone())
            .collect::<Vec<_>>();
        let removed_session_id =
            destroyed_sessions
                .iter()
                .find_map(|(destroyed_session, session_id)| {
                    (destroyed_session == &session_name).then_some(*session_id)
                });
        let session_destroyed = removed_session_id.is_some();
        let removed_pane_ids = self.pane_ids_no_longer_referenced([pane_id]);

        Ok(KilledPaneResult {
            response: KillPaneResponse {
                target,
                window_destroyed: true,
            },
            hook_context,
            session_destroyed,
            removed_session_id,
            removed_pane_ids,
            affected_sessions,
            destroyed_sessions,
        })
    }

    fn commit_linked_window_removals(
        &mut self,
        direct_slots: &[WindowLinkSlot],
        removals: &[LinkedWindowRemoval],
    ) -> Result<Vec<(SessionName, u32)>, RmuxError> {
        for slot in direct_slots {
            let _ = self.detach_window_link_slot(&slot.session_name, slot.window_index);
        }

        let mut destroyed_sessions = Vec::new();
        let mut surviving_sessions = Vec::new();
        for removal in removals {
            let window_target =
                WindowTarget::with_window(removal.session_name.clone(), removal.window_index);
            self.clear_auto_named_window(&removal.session_name, removal.window_index);
            if removal.destroy_session {
                let removed = self.sessions.remove_session(&removal.session_name)?;
                let _ = self.options.remove_session(&removal.session_name);
                let _ = self.environment.remove_session(&removal.session_name);
                let _ = self.hooks.remove_session(&removal.session_name);
                destroyed_sessions.push((removal.session_name.clone(), removed.id().as_u32()));
            } else {
                self.sessions
                    .session_mut(&removal.session_name)
                    .ok_or_else(|| session_not_found(&removal.session_name))?
                    .remove_window(removal.window_index)?;
                let _ = self.options.remove_window(&window_target);
                let _ = self.hooks.remove_window(&window_target);
                surviving_sessions.push(removal.session_name.clone());
            }
        }

        surviving_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        surviving_sessions.dedup();
        for surviving_session in surviving_sessions {
            self.synchronize_session_group_from(&surviving_session)?;
            self.sync_pane_lifecycle_dimensions_for_session(&surviving_session);
        }
        Ok(destroyed_sessions)
    }

    fn remove_destroyed_linked_session_runtime(&mut self, session_name: &SessionName) {
        self.remove_window_link_session_slots(session_name);
        #[cfg(windows)]
        let _ = self.starting_panes.remove(session_name);
        for pipe in self.remove_session_pipes(session_name).into_values() {
            pipe.stop();
        }
        let mut removed_outputs = self.remove_session_pane_outputs(session_name);
        removed_outputs.abort_output_readers();
        let _ = self.dead_panes.remove(session_name);
        let _ = self.attached_submitted_rows.remove(session_name);
        let _ = self.attached_terminal_pixels.remove(session_name);
        self.auto_named_windows
            .retain(|(tracked_session, _)| tracked_session != session_name);
        if let Some(mut terminals) = self.terminals.remove_session(session_name) {
            for terminal in terminals.drain().map(|(_, terminal)| terminal) {
                terminal.terminate_in_background();
            }
        }
    }
}

fn linked_window_removals(
    state: &HandlerState,
    direct_slots: &[WindowLinkSlot],
    pane_id: PaneId,
) -> Result<Vec<LinkedWindowRemoval>, RmuxError> {
    let mut pending = direct_slots.to_vec();
    let mut seen = HashSet::new();
    let mut removals = Vec::new();
    while let Some(slot) = pending.pop() {
        if !seen.insert(slot.clone()) {
            continue;
        }
        for member in state.sessions.session_group_members(&slot.session_name) {
            pending.push(WindowLinkSlot {
                session_name: member,
                window_index: slot.window_index,
            });
        }
        for linked_slot in state.window_link_slots_for(&slot.session_name, slot.window_index) {
            pending.push(linked_slot);
        }

        let session = state
            .sessions
            .session(&slot.session_name)
            .ok_or_else(|| session_not_found(&slot.session_name))?;
        let window = session.window_at(slot.window_index).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{}", slot.session_name, slot.window_index),
                "window index does not exist in linked session",
            )
        })?;
        let linked_pane_id = window
            .panes()
            .first()
            .filter(|_| window.pane_count() == 1)
            .map(|pane| pane.id())
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "linked window {}:{} no longer contains exactly one pane",
                    slot.session_name, slot.window_index
                ))
            })?;
        if linked_pane_id != pane_id {
            return Err(RmuxError::Server(format!(
                "linked window {}:{} resolves to pane {} instead of {}",
                slot.session_name,
                slot.window_index,
                linked_pane_id.as_u32(),
                pane_id.as_u32()
            )));
        }
        removals.push(LinkedWindowRemoval {
            session_name: slot.session_name,
            window_index: slot.window_index,
            destroy_session: session.windows().len() == 1,
        });
    }

    removals.sort_by(|left, right| {
        left.session_name
            .as_str()
            .cmp(right.session_name.as_str())
            .then_with(|| left.window_index.cmp(&right.window_index))
    });
    Ok(removals)
}

fn restore_linked_pane_runtime(
    state: &mut HandlerState,
    runtime_session_name: &SessionName,
    removed_terminals: HashMap<PaneId, crate::pane_terminal_process::PaneTerminal>,
    removed_outputs: RemovedPaneOutputs,
    source_error: &RmuxError,
) -> Result<(), RmuxError> {
    if !removed_terminals.is_empty() {
        state
            .terminals
            .insert_existing_panes(runtime_session_name, removed_terminals)
            .map_err(|rollback_error| {
                RmuxError::Server(format!(
                    "failed to restore linked pane runtime in {runtime_session_name} after {source_error}: {rollback_error}"
                ))
            })?;
    }
    state.insert_existing_pane_outputs(runtime_session_name, removed_outputs);
    Ok(())
}
