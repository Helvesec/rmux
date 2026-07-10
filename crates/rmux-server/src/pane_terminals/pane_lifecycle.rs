use std::path::Path;

use rmux_core::{PaneId, Session};
use rmux_proto::{
    KillPaneResponse, PaneTarget, ProcessCommand, RespawnPaneRequest, RespawnPaneResponse,
    RmuxError, SessionName,
};
use rmux_pty::PtyMaster;

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_lookup::initial_pane;
use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::{validate_process_command, SessionBaseEnvironment, TerminalProfile};

use super::lifecycle_state::terminal_size_from_geometry;
use super::{
    pane_terminal_geometry_for_session, session_not_found, HandlerState, InitialPaneSpawnOptions,
    KilledPaneHookContext, KilledPaneResult, PaneLifecycleSpawn, PaneOutputSpawn,
    WindowSpawnOptions,
};

#[path = "pane_lifecycle/preview.rs"]
mod preview;

#[path = "pane_lifecycle/split.rs"]
mod split;

use preview::preview_kill_pane;

pub(in crate::pane_terminals) struct PreparedWindowTerminal {
    terminal: PaneTerminal,
    output: PaneOutputSpawn,
    lifecycle: PaneLifecycleSpawn,
    automatic_window_name: Option<String>,
    pane_index: u32,
}

impl PreparedWindowTerminal {
    pub(in crate::pane_terminals) fn automatic_window_name(&self) -> Option<&str> {
        self.automatic_window_name.as_deref()
    }
}

impl HandlerState {
    pub(in crate::pane_terminals) fn prepare_window_terminal(
        &self,
        session: &Session,
        window_index: u32,
        spawn: WindowSpawnOptions<'_>,
        base_environment: Option<&SessionBaseEnvironment>,
    ) -> Result<PreparedWindowTerminal, RmuxError> {
        let window = session.window_at(window_index).ok_or_else(|| {
            RmuxError::invalid_target(
                format!("{}:{window_index}", session.name()),
                "window index does not exist in session",
            )
        })?;
        let pane = window.pane(0).ok_or_else(|| {
            RmuxError::Server(format!(
                "initial pane missing for session {}:{window_index}",
                session.name()
            ))
        })?;
        let pane_geometry = pane_terminal_geometry_for_session(
            session,
            &self.options,
            window_index,
            pane.geometry(),
        );
        let profile = TerminalProfile::for_session_with_base_environment(
            &self.environment,
            &self.options,
            session.name(),
            session.id().as_u32(),
            spawn.socket_path,
            base_environment,
            spawn.spawn_environment,
            true,
            spawn.environment_overrides,
            Some(pane.id()),
            spawn
                .start_directory
                .filter(|path| !path.as_os_str().is_empty())
                .or(session.cwd()),
        )?;
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let mut terminal =
            open_pane_terminal(pane_geometry, profile, runtime_window_name, spawn.command)?;
        let pid = terminal.pid();
        let output_reader =
            clone_terminal_for_output_reader(&mut terminal, session.name(), pane.id())?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, session.name(), pane.id())?;
        #[cfg(unix)]
        let _ = self.pane_reader_runtime()?;

        Ok(PreparedWindowTerminal {
            terminal,
            output: PaneOutputSpawn {
                geometry: pane_geometry,
                initial_title,
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback: spawn.pane_alert_callback,
                pane_exit_callback: spawn.pane_exit_callback,
            },
            lifecycle: PaneLifecycleSpawn {
                session_id: session.id(),
                window_id: window.id(),
                pane_id: pane.id(),
                command: spawn.command.map(ProcessCommand::display_command),
                working_directory: Some(lifecycle_cwd),
                private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
                dimensions: terminal_size_from_geometry(pane_geometry),
                pid: Some(pid),
            },
            automatic_window_name,
            pane_index: pane.index(),
        })
    }

    pub(in crate::pane_terminals) fn install_prepared_window_terminal(
        &mut self,
        runtime_session_name: &SessionName,
        window_index: u32,
        prepared: PreparedWindowTerminal,
    ) -> Result<PaneId, RmuxError> {
        let PreparedWindowTerminal {
            terminal,
            output,
            lifecycle,
            automatic_window_name: _,
            pane_index,
        } = prepared;
        let pane_id = lifecycle.pane_id;
        self.terminals.insert_pane(
            runtime_session_name.clone(),
            pane_id,
            window_index,
            pane_index,
            terminal,
        )?;
        if let Err(error) = self.reset_pane_output(runtime_session_name, pane_id, output) {
            if let Some(terminal) = self.terminals.remove_pane(runtime_session_name, pane_id) {
                terminal.terminate_in_background();
            }
            return Err(error);
        }
        self.record_pane_lifecycle_spawn(lifecycle);
        let output_sequence = self.pane_output_generation(runtime_session_name, pane_id);
        self.update_pane_lifecycle_output_sequence(pane_id, output_sequence);
        Ok(pane_id)
    }

    pub(crate) fn insert_initial_session_terminal(
        &mut self,
        session_name: &SessionName,
        spawn: InitialPaneSpawnOptions<'_>,
    ) -> Result<(), RmuxError> {
        let pane = initial_pane(&self.sessions, session_name)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(session_name, pane.window_index);
        let (session_id, window_id, requested_cwd, pane_geometry) = {
            let session = self
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let window = session.window_at(pane.window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{session_name}:{}", pane.window_index),
                    "window index does not exist in session",
                )
            })?;
            (
                session.id(),
                window.id(),
                session.cwd(),
                pane_terminal_geometry_for_session(
                    session,
                    &self.options,
                    pane.window_index,
                    pane.geometry,
                ),
            )
        };
        let profile = TerminalProfile::for_initial_session_pane(
            &self.environment,
            &self.options,
            session_name,
            session_id.as_u32(),
            spawn.socket_path,
            spawn.spawn_environment,
            spawn.raw_spawn_environment,
            true,
            spawn.environment_overrides,
            Some(pane.id),
            requested_cwd,
        )?;
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let mut terminal = open_pane_terminal(
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            spawn.command,
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&mut terminal, session_name, pane.id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, session_name, pane.id)?;

        self.apply_automatic_window_name(session_name, pane.window_index, automatic_window_name)?;

        self.terminals
            .insert_session(runtime_session_name.clone(), pane.id, terminal)?;
        if let Err(error) = self.insert_pane_output(
            &runtime_session_name,
            pane.id,
            PaneOutputSpawn {
                geometry: pane_geometry,
                initial_title,
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback: spawn.pane_alert_callback,
                pane_exit_callback: spawn.pane_exit_callback,
            },
        ) {
            let _ = self.terminals.remove_session(&runtime_session_name);
            return Err(error);
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: pane.id,
            command: spawn.command.map(ProcessCommand::display_command),
            working_directory: Some(lifecycle_cwd),
            private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
            dimensions: terminal_size_from_geometry(pane.geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane.id);
        self.update_pane_lifecycle_output_sequence(pane.id, output_sequence);

        Ok(())
    }

    pub(crate) fn resize_terminals(&mut self, session_name: &SessionName) -> Result<(), RmuxError> {
        let session_size = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?
            .window()
            .size();
        let terminal_pixels = self.attached_terminal_pixels.get(session_name).copied();
        for (runtime_session_name, pane_geometries) in
            self.session_pane_terminal_geometries_by_runtime(session_name)?
        {
            self.terminals.resize_session(
                &runtime_session_name,
                &pane_geometries,
                session_size,
                terminal_pixels,
            )?;
            self.resize_transcripts(&runtime_session_name, &pane_geometries);
        }
        self.sync_pane_lifecycle_dimensions_for_session(session_name);
        Ok(())
    }

    pub(crate) fn insert_window_terminal(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        spawn: WindowSpawnOptions<'_>,
    ) -> Result<(), RmuxError> {
        self.spawn_window_terminal(session_name, window_index, spawn, None)
    }

    fn spawn_window_terminal(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        spawn: WindowSpawnOptions<'_>,
        base_environment_override: Option<&SessionBaseEnvironment>,
    ) -> Result<(), RmuxError> {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let (session_id, window_id, pane_id, pane_index, pane_geometry, requested_cwd) = {
            let session = self
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            let window = session.window_at(window_index).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{session_name}:{window_index}"),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(0).ok_or_else(|| {
                RmuxError::Server(format!(
                    "initial pane missing for session {session_name}:{window_index}"
                ))
            })?;
            (
                session.id(),
                window.id(),
                pane.id(),
                pane.index(),
                pane_terminal_geometry_for_session(
                    session,
                    &self.options,
                    window_index,
                    pane.geometry(),
                ),
                session.cwd().map(Path::to_path_buf),
            )
        };
        let captured_base_environment =
            self.session_base_environment_for_window(session_name, window_index);
        let base_environment = base_environment_override.or(captured_base_environment.as_ref());
        let profile = TerminalProfile::for_session_with_base_environment(
            &self.environment,
            &self.options,
            session_name,
            session_id.as_u32(),
            spawn.socket_path,
            base_environment,
            spawn.spawn_environment,
            true,
            spawn.environment_overrides,
            Some(pane_id),
            spawn
                .start_directory
                .filter(|path| !path.as_os_str().is_empty())
                .or(requested_cwd.as_deref()),
        )?;
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let mut terminal = open_pane_terminal(
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            spawn.command,
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&mut terminal, session_name, pane_id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, session_name, pane_id)?;

        self.apply_automatic_window_name(session_name, window_index, automatic_window_name)?;

        self.terminals.insert_pane(
            runtime_session_name.clone(),
            pane_id,
            window_index,
            pane_index,
            terminal,
        )?;
        let output_spawn = PaneOutputSpawn {
            geometry: pane_geometry,
            initial_title,
            output_reader,
            #[cfg(windows)]
            exit_watcher: Some(exit_watcher),
            pane_alert_callback: spawn.pane_alert_callback,
            pane_exit_callback: spawn.pane_exit_callback,
        };
        let output_result = self.insert_pane_output(&runtime_session_name, pane_id, output_spawn);
        if let Err(error) = output_result {
            let _ = self.terminals.remove_pane(&runtime_session_name, pane_id);
            return Err(error);
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id,
            command: spawn.command.map(ProcessCommand::display_command),
            working_directory: Some(lifecycle_cwd),
            private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
            dimensions: terminal_size_from_geometry(pane_geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane_id);
        self.update_pane_lifecycle_output_sequence(pane_id, output_sequence);

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn kill_pane(&mut self, target: PaneTarget) -> Result<KilledPaneResult, RmuxError> {
        self.kill_pane_with_options(target, false)
    }

    pub(crate) fn kill_pane_with_options(
        &mut self,
        target: PaneTarget,
        kill_all_except: bool,
    ) -> Result<KilledPaneResult, RmuxError> {
        let session_name = target.session_name().clone();
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let (hook_context, pane_id, remove_session) = {
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
            (
                hook_context,
                pane_id,
                !kill_all_except
                    && previous_session.windows().len() == 1
                    && window.pane_count() == 1,
            )
        };
        if remove_session {
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
            });
        }

        let runtime_session_name = self.runtime_session_name(&session_name);
        let preview_outcome = preview_kill_pane(&self.sessions, &target, kill_all_except)?;
        self.ensure_panes_exist(&session_name, preview_outcome.removed_pane_ids())?;

        let committed_outcome = {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            if kill_all_except {
                session.kill_other_panes_in_window(target.window_index(), target.pane_index())?
            } else {
                session.kill_pane_in_window(target.window_index(), target.pane_index())?
            }
        };
        debug_assert_eq!(committed_outcome, preview_outcome);
        let removed_pane_ids = committed_outcome.removed_pane_ids().to_vec();
        for pane_id in committed_outcome.removed_pane_ids() {
            self.clear_marked_pane_if_id(*pane_id);
        }

        let mut removed_terminals = match self
            .terminals
            .remove_pane_batch(&runtime_session_name, committed_outcome.removed_pane_ids())
        {
            Ok(removed_terminals) => removed_terminals,
            Err(error) => {
                self.replace_session(&session_name, previous_session)?;
                return Err(error);
            }
        };
        let mut removed_outputs =
            self.remove_pane_outputs(&runtime_session_name, committed_outcome.removed_pane_ids());

        if let Err(error) = self.resize_terminals(&session_name) {
            self.restore_session_and_panes_after_resize_error(
                &session_name,
                previous_session,
                removed_terminals,
                removed_outputs,
                &error,
            )?;
            return Err(error);
        }
        removed_outputs.abort_output_readers();
        terminate_removed_terminals(&mut removed_terminals);
        self.remove_pane_lifecycles(committed_outcome.removed_pane_ids());

        self.synchronize_session_group_from(&session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);
        self.rekey_pane_options_after_session_change(&before_pane_options, &session_name)?;

        if committed_outcome.window_destroyed() {
            let _ = self
                .options
                .remove_window(&rmux_proto::WindowTarget::with_window(
                    session_name.clone(),
                    target.window_index(),
                ));
        }

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
        })
    }

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
            start_directory,
            environment,
            command,
            process_command,
        } = request;
        let process_command = process_command
            .or_else(|| crate::legacy_command::from_legacy_command(command.as_deref()));
        validate_process_command(process_command.as_ref())?;
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
                pane_terminal_geometry_for_session(
                    session,
                    &self.options,
                    window_index,
                    pane.geometry(),
                ),
                session.cwd().map(Path::to_path_buf),
            )
        };

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
        let profile = TerminalProfile::for_session_with_base_environment(
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
        let automatic_window_name = profile.automatic_window_name(process_command.as_ref());
        let runtime_window_name = profile.runtime_window_name(process_command.as_ref());
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let mut terminal = open_pane_terminal(
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
        self.apply_automatic_window_name(&session_name, window_index, automatic_window_name)?;
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id,
            command: process_command
                .as_ref()
                .map(ProcessCommand::display_command),
            working_directory: Some(lifecycle_cwd),
            private_environment: environment,
            dimensions: terminal_size_from_geometry(pane_geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane_id);
        self.update_pane_lifecycle_output_sequence(pane_id, output_sequence);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(RespawnPaneResponse { target })
    }
}

pub(in crate::pane_terminals) fn terminate_removed_terminals(
    terminals: &mut std::collections::HashMap<PaneId, crate::pane_terminal_process::PaneTerminal>,
) {
    for terminal in terminals.drain().map(|(_, terminal)| terminal) {
        terminal.terminate_in_background();
    }
}

pub(in crate::pane_terminals) fn clone_terminal_for_output_reader(
    terminal: &mut PaneTerminal,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<PtyMaster, RmuxError> {
    terminal.clone_master_for_output_reader().map_err(|error| {
        RmuxError::Server(format!(
            "failed to clone pane output reader for pane id {} in session {}: {error}",
            pane_id.as_u32(),
            session_name
        ))
    })
}

#[cfg(windows)]
pub(in crate::pane_terminals) fn clone_terminal_for_exit_watcher(
    terminal: &PaneTerminal,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<rmux_pty::PtyChild, RmuxError> {
    terminal.clone_child_for_wait().map_err(|error| {
        RmuxError::Server(format!(
            "failed to clone pane exit watcher for pane id {} in session {}: {error}",
            pane_id.as_u32(),
            session_name
        ))
    })
}
