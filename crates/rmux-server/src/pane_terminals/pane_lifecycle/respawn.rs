use std::path::Path;

use rmux_proto::{RespawnPaneRequest, RespawnPaneResponse, RmuxError};

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_process::open_pane_terminal;
use crate::terminal::{validate_process_command, TerminalProfile};

use super::super::lifecycle_state::terminal_size_from_geometry;
use super::super::{
    session_not_found, HandlerState, KilledPaneHookContext, PaneLifecycleSpawn, PaneOutputSpawn,
    PaneWindowGeometry, WindowNameApplication,
};
#[cfg(windows)]
use super::clone_terminal_for_exit_watcher;
use super::clone_terminal_for_output_reader;

impl HandlerState {
    pub(crate) fn respawn_pane(
        &mut self,
        request: RespawnPaneRequest,
        socket_path: &Path,
        spawn_environment: Option<&std::collections::HashMap<String, String>>,
        pane_alert_callback: Option<PaneAlertCallback>,
        pane_exit_callback: Option<PaneExitCallback>,
        mut on_replaced_active_pane: impl FnMut(&mut Self, &KilledPaneHookContext),
    ) -> Result<RespawnPaneResponse, RmuxError> {
        let RespawnPaneRequest {
            target,
            kill,
            mut start_directory,
            mut environment,
            command,
            process_command,
        } = request;
        let requested_process_command = process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        validate_process_command(requested_process_command.as_ref())?;
        let session_name = target.session_name().clone();
        let window_index = target.window_index();
        let pane_index = target.pane_index();
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let (session_id, window_id, window_name, pane_id, pane_geometry, requested_cwd) = {
            let session = self
                .sessions
                .session(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{session_name}:{window_index}"),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(pane_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    target.to_string(),
                    "pane index does not exist in session",
                )
            })?;
            (
                session.id(),
                window.id(),
                window.name().unwrap_or_default().to_owned(),
                pane.id(),
                PaneWindowGeometry::new(session.name(), window_index, window).content_geometry(
                    &self.options,
                    pane.index(),
                    pane.geometry(),
                    false,
                    false,
                ),
                session.cwd().map(Path::to_path_buf),
            )
        };

        let provenance = self.pane_respawn_provenance(pane_id);
        let process_command = requested_process_command.or_else(|| {
            provenance
                .as_ref()
                .and_then(|provenance| provenance.process_command.clone())
        });
        validate_process_command(process_command.as_ref())?;
        if start_directory.is_none() {
            start_directory = provenance
                .as_ref()
                .and_then(|provenance| provenance.working_directory.clone());
        }
        let respawn_environment = provenance
            .as_ref()
            .map(|provenance| provenance.private_environment.clone())
            .unwrap_or_else(|| environment.clone().unwrap_or_default());
        if environment.is_none() {
            environment = Some(respawn_environment.clone());
        }

        #[cfg(windows)]
        let pane_was_starting =
            self.pane_is_starting_in_window(&session_name, window_index, pane_index);
        #[cfg(not(windows))]
        let pane_was_starting = false;

        let pane_was_alive = !pane_was_starting
            && self.terminals.pane_is_alive(
                &runtime_session_name,
                pane_id,
                window_index,
                pane_index,
            )?;
        if (pane_was_starting || pane_was_alive) && !kill {
            return Err(RmuxError::ProcessStillRunning);
        }
        let base_environment = self.session_base_environment_for_pane_target(&target);
        let mut profile = TerminalProfile::for_session_with_base_environment(
            &self.environment,
            &self.options,
            &session_name,
            session_id.as_u32(),
            socket_path,
            base_environment.as_ref(),
            spawn_environment,
            true,
            environment.as_deref(),
            Some(pane_id),
            start_directory.as_deref().or(requested_cwd.as_deref()),
        )?;
        if let Some(provenance) = provenance.as_ref() {
            profile = profile.with_respawn_shell(provenance.shell.clone());
        }
        let automatic_window_name = profile.automatic_window_name(process_command.as_ref());
        let runtime_window_name = profile.runtime_window_name(process_command.as_ref());
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = open_pane_terminal(
            self.pane_shell.0.as_ref(),
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            process_command.as_ref(),
        )?;
        let pid = terminal.pid();
        let output_reader =
            clone_terminal_for_output_reader(&mut terminal, &session_name, pane_id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, &session_name, pane_id)?;

        #[cfg(windows)]
        if pane_was_starting {
            // Keep the deferred pane and its accepted input intact until every
            // fallible profile/open/clone step for the replacement succeeds.
            // A rejected respawn must leave the original pane able to finish
            // startup and flush its queued input.
            let _ = self.cancel_starting_pane(&runtime_session_name, pane_id);
            on_replaced_active_pane(
                self,
                &KilledPaneHookContext {
                    target: target.clone(),
                    pane_id: pane_id.as_u32(),
                    window_id: window_id.as_u32(),
                    window_name: window_name.clone(),
                },
            );
        }

        if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, pane_id) {
            pipe.stop();
        }
        if let Some(terminal) = self.terminals.remove_pane(&runtime_session_name, pane_id) {
            terminal.terminate_in_background();
            if pane_was_alive {
                on_replaced_active_pane(
                    self,
                    &KilledPaneHookContext {
                        target: target.clone(),
                        pane_id: pane_id.as_u32(),
                        window_id: window_id.as_u32(),
                        window_name: window_name.clone(),
                    },
                );
            }
        }
        self.terminals.insert_pane(
            runtime_session_name.clone(),
            pane_id,
            window_index,
            pane_index,
            terminal,
        )?;
        self.reset_pane_output(
            &runtime_session_name,
            pane_id,
            PaneOutputSpawn {
                geometry: pane_geometry,
                initial_title,
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback,
                pane_exit_callback,
            },
        )?;
        self.apply_window_name(
            &session_name,
            window_index,
            automatic_window_name,
            WindowNameApplication::AutomaticUpdate,
        )?;
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id,
            process_command,
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: environment,
            respawn_environment: Some(respawn_environment),
            dimensions: terminal_size_from_geometry(pane_geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane_id);
        self.update_pane_lifecycle_output_sequence(pane_id, output_sequence);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);
        #[cfg(test)]
        self.publish_seeded_respawn_startup_output_for_test(&target)?;

        Ok(RespawnPaneResponse { target })
    }
}

#[cfg(test)]
impl HandlerState {
    /// Make the next replacement runtime publish `bytes` as startup output
    /// before `respawn_pane` returns, so before any caller reconciles focus.
    ///
    /// This models the pinned ConPTY pair (OpenConsole/conpty.dll), whose
    /// startup preamble carries DEC 1004h and whose reader may parse it before
    /// the respawn reconciles focus. A Unix PTY running a quiet child is
    /// silent, so without this seed only Windows reaches that outcome.
    pub(crate) fn seed_next_respawn_startup_output_for_test(&mut self, bytes: &[u8]) {
        self.respawn_startup_output_for_test = Some(bytes.to_vec());
    }

    fn publish_seeded_respawn_startup_output_for_test(
        &mut self,
        target: &rmux_proto::PaneTarget,
    ) -> Result<(), RmuxError> {
        let Some(bytes) = self.respawn_startup_output_for_test.take() else {
            return Ok(());
        };
        self.append_bytes_to_pane_transcript_for_test(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            &bytes,
        )
    }
}
