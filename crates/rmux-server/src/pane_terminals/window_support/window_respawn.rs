use rmux_proto::RmuxError;

use super::{session_not_found, window_pane_ids, HandlerState, RespawnWindowResult};
use crate::pane_terminals::{
    terminate_removed_terminals, RespawnWindowOptions, WindowSpawnOptions,
};
use crate::terminal::validate_process_command;

impl HandlerState {
    pub(crate) fn respawn_window(
        &mut self,
        target: rmux_proto::WindowTarget,
        options: RespawnWindowOptions<'_>,
    ) -> Result<RespawnWindowResult, RmuxError> {
        let RespawnWindowOptions { kill, spawn } = options;
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);

        let session = self
            .sessions
            .session(&session_name)
            .ok_or_else(|| session_not_found(&session_name))?;
        let pane_ids = window_pane_ids(session, &session_name, window_index)?;
        let previous_window = self.sessions.checkpoint_window(&target)?;

        // Remain-on-exit retains terminals after their processes have exited.
        // Use the same process check as respawn-pane, including pending spawns.
        if !kill {
            let window = session
                .window_at(window_index)
                .expect("window was resolved");
            for pane in window.panes() {
                #[cfg(windows)]
                let starting =
                    self.pane_is_starting_in_window(&session_name, window_index, pane.index());
                #[cfg(not(windows))]
                let starting = false;
                if starting
                    || self.terminals.pane_is_alive(
                        &runtime_session_name,
                        pane.id(),
                        window_index,
                        pane.index(),
                    )?
                {
                    return Err(RmuxError::Server(
                        "window still active; use -k to force respawn".to_owned(),
                    ));
                }
            }
        }

        let pane_id = pane_ids
            .first()
            .copied()
            .ok_or_else(|| RmuxError::Server("window has no panes".to_owned()))?;
        let provenance = self.pane_respawn_provenance(pane_id);
        let process_command = spawn.command.cloned().or_else(|| {
            provenance
                .as_ref()
                .and_then(|provenance| provenance.process_command.clone())
        });
        validate_process_command(process_command.as_ref())?;
        let start_directory = spawn
            .start_directory
            .map(std::path::Path::to_path_buf)
            .or_else(|| {
                provenance
                    .as_ref()
                    .and_then(|provenance| provenance.working_directory.clone())
            });
        let respawn_environment = provenance
            .as_ref()
            .map(|provenance| provenance.private_environment.clone());
        let respawn_shell = provenance
            .as_ref()
            .map(|provenance| provenance.shell.as_path());
        let environment_overrides = spawn
            .environment_overrides
            .map(<[String]>::to_vec)
            .or_else(|| respawn_environment.clone());
        let spawn = WindowSpawnOptions {
            start_directory: start_directory.as_deref(),
            command: process_command.as_ref(),
            socket_path: spawn.socket_path,
            spawn_environment: spawn.spawn_environment,
            environment_overrides: environment_overrides.as_deref(),
            respawn_shell,
            respawn_environment: respawn_environment.as_deref(),
            pane_alert_callback: spawn.pane_alert_callback,
            pane_exit_callback: spawn.pane_exit_callback,
        };
        let removed_pane_ids = pane_ids
            .iter()
            .copied()
            .filter(|id| *id != pane_id)
            .collect::<Vec<_>>();
        let previous_output_generation =
            self.pane_output_generation(&runtime_session_name, pane_id);
        let base_environment =
            self.session_base_environment_for_window(&session_name, window_index);
        let mut pane_option_sessions =
            self.window_linked_session_family_list(&session_name, window_index);
        if pane_option_sessions.is_empty() {
            pane_option_sessions.push(session_name.clone());
        }
        pane_option_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        pane_option_sessions.dedup();
        let before_pane_options = pane_option_sessions
            .into_iter()
            .map(|affected_session| {
                self.pane_option_slots_for_session(&affected_session)
                    .map(|snapshot| (affected_session, snapshot))
            })
            .collect::<Result<Vec<_>, RmuxError>>()?;

        // Prepare the replacement process against a preview of the new layout.
        // No old runtime state is touched until every fallible spawn step has
        // succeeded, so profile/PTY/output-reader failures are true no-ops.
        let respawned_window = self.sessions.preview_respawn_window(&target, pane_id)?;
        let prepared = self.prepare_window_terminal(
            self.sessions
                .session(&session_name)
                .expect("validated session"),
            window_index,
            &respawned_window,
            spawn,
            base_environment.as_ref(),
        )?;
        let automatic_name = prepared
            .automatic_window_name()
            .filter(|_| {
                respawned_window.name().is_none()
                    && crate::automatic_rename::window_allows_automatic_rename(
                        &self.options,
                        &session_name,
                        window_index,
                        &respawned_window,
                        self.tracks_auto_named_window(&session_name, window_index),
                    )
            })
            .map(str::to_owned);
        let automatic_name_applied = automatic_name.is_some();

        let mut removed_terminals = pane_ids
            .iter()
            .filter_map(|pane_id| {
                self.terminals
                    .remove_pane(&runtime_session_name, *pane_id)
                    .map(|terminal| (*pane_id, terminal))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let mut removed_outputs = self.remove_pane_outputs(&runtime_session_name, &pane_ids);
        // Keep the output channel for the stable pane identity. Existing SDK
        // subscribers own receivers for this sender; replacing the channel
        // would leave a registry record that can never observe the respawned
        // process. Generation advancement below rejects any late output from
        // the old reader while preserving the receiver identity.
        let retained_output_sender = removed_outputs.pane_output_sender(pane_id);
        let retained_colours = removed_outputs.control_colours(pane_id);
        self.seed_pane_output_generation(
            &runtime_session_name,
            pane_id,
            previous_output_generation,
        );
        self.sessions.respawn_window(&target, pane_id)?;
        self.sessions.select_window(&target)?;
        if let Some(name) = automatic_name {
            let _ = self.sessions.set_automatic_window_name(&target, name)?;
        }

        if let Err(error) = self.install_prepared_window_terminal(
            &runtime_session_name,
            window_index,
            prepared,
            retained_output_sender,
            retained_colours,
        ) {
            self.sessions
                .restore_window_checkpoint(&target, previous_window)?;
            self.terminals
                .insert_existing_panes(&runtime_session_name, removed_terminals)?;
            self.insert_existing_pane_outputs(&runtime_session_name, removed_outputs);
            return Err(error);
        }

        for old_pane_id in &pane_ids {
            if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, *old_pane_id) {
                pipe.stop();
            }
        }
        for removed_pane_id in &removed_pane_ids {
            self.clear_marked_pane_if_id(*removed_pane_id);
        }
        self.remove_pane_lifecycles(&removed_pane_ids);
        if automatic_name_applied {
            self.mark_auto_named_window(&session_name, window_index);
        }
        removed_outputs.abort_output_readers();
        terminate_removed_terminals(&mut removed_terminals);

        // The window model is shared through explicit link aliases as well as
        // session groups. Synchronize every alias before deciding which pane
        // identities disappeared; otherwise a linked slot can keep destroyed
        // sibling panes artificially reachable after the runtime was replaced.
        let mut synchronized_sessions =
            self.synchronize_linked_window_family_from_slot(&session_name, window_index)?;
        synchronized_sessions.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        synchronized_sessions.dedup();
        for synchronized_session in &synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(synchronized_session);
        }
        for (affected_session, before) in before_pane_options {
            self.rekey_pane_options_after_session_change(&before, &affected_session)?;
        }
        let removed_pane_ids = self.pane_ids_no_longer_referenced(removed_pane_ids);

        Ok(RespawnWindowResult {
            response: rmux_proto::RespawnWindowResponse { target },
            retained_pane_id: pane_id,
            removed_pane_ids,
            refresh_sessions: synchronized_sessions,
        })
    }
}
