use std::path::Path;

use rmux_core::PaneId;
use rmux_proto::{
    KillPaneResponse, PaneTarget, RespawnPaneRequest, RespawnPaneResponse, RmuxError, SessionName,
    SplitDirection, SplitWindowResponse, SplitWindowTarget,
};
use rmux_pty::PtyMaster;

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_lookup::{initial_pane, missing_pane_terminal};
use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::TerminalProfile;

use super::lifecycle_state::terminal_size_from_geometry;
use super::{
    pane_terminal_geometry_for_session, session_not_found, HandlerState, KilledPaneHookContext,
    KilledPaneResult, PaneLifecycleSpawn, PaneOutputSpawn, WindowSpawnOptions,
};

#[path = "pane_lifecycle/preview.rs"]
mod preview;

use preview::{
    preview_kill_pane, preview_split, split_window_internal_direction, split_window_session_name,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneOutputMode {
    Insert,
    Reset,
}

impl HandlerState {
    pub(crate) fn insert_initial_session_terminal(
        &mut self,
        session_name: &SessionName,
        socket_path: &Path,
        environment_overrides: Option<&[String]>,
        command: Option<&[String]>,
        pane_alert_callback: Option<PaneAlertCallback>,
        pane_exit_callback: Option<PaneExitCallback>,
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
                pane_terminal_geometry_for_session(session, &self.options, pane.geometry),
            )
        };
        let profile = TerminalProfile::for_session(
            &self.environment,
            &self.options,
            session_name,
            session_id.as_u32(),
            socket_path,
            true,
            environment_overrides,
            Some(pane.id),
            requested_cwd,
        )?;
        let automatic_window_name = profile.automatic_window_name(command);
        let runtime_window_name = profile.runtime_window_name(command);
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let terminal =
            open_pane_terminal(pane_geometry, profile, runtime_window_name.clone(), command)?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&terminal, session_name, pane.id)?;
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
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback,
                pane_exit_callback,
            },
        ) {
            let _ = self.terminals.remove_session(&runtime_session_name);
            return Err(error);
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: pane.id,
            command: command.map(<[String]>::to_vec),
            working_directory: Some(lifecycle_cwd),
            private_environment: environment_overrides.map(<[String]>::to_vec),
            dimensions: terminal_size_from_geometry(pane.geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane.id);
        self.update_pane_lifecycle_output_sequence(pane.id, output_sequence);

        Ok(())
    }

    pub(crate) fn resize_terminals(&mut self, session_name: &SessionName) -> Result<(), RmuxError> {
        for (runtime_session_name, pane_geometries) in
            self.session_pane_terminal_geometries_by_runtime(session_name)?
        {
            self.terminals
                .resize_session(&runtime_session_name, &pane_geometries)?;
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
        self.spawn_window_terminal(session_name, window_index, spawn, PaneOutputMode::Insert)
    }

    pub(in crate::pane_terminals) fn reset_window_terminal(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        spawn: WindowSpawnOptions<'_>,
    ) -> Result<(), RmuxError> {
        self.spawn_window_terminal(session_name, window_index, spawn, PaneOutputMode::Reset)
    }

    fn spawn_window_terminal(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        spawn: WindowSpawnOptions<'_>,
        output_mode: PaneOutputMode,
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
                pane_terminal_geometry_for_session(session, &self.options, pane.geometry()),
                session.cwd(),
            )
        };
        let profile = TerminalProfile::for_session(
            &self.environment,
            &self.options,
            session_name,
            session_id.as_u32(),
            spawn.socket_path,
            true,
            spawn.environment_overrides,
            Some(pane_id),
            spawn
                .start_directory
                .filter(|path| !path.as_os_str().is_empty())
                .or(requested_cwd),
        )?;
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let terminal = open_pane_terminal(
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            spawn.command,
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&terminal, session_name, pane_id)?;
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
            output_reader,
            #[cfg(windows)]
            exit_watcher: Some(exit_watcher),
            pane_alert_callback: spawn.pane_alert_callback,
            pane_exit_callback: spawn.pane_exit_callback,
        };
        let output_result = match output_mode {
            PaneOutputMode::Insert => {
                self.insert_pane_output(&runtime_session_name, pane_id, output_spawn)
            }
            PaneOutputMode::Reset => {
                self.reset_pane_output(&runtime_session_name, pane_id, output_spawn)
            }
        };
        if let Err(error) = output_result {
            let _ = self.terminals.remove_pane(&runtime_session_name, pane_id);
            return Err(error);
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id,
            command: spawn.command.map(<[String]>::to_vec),
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
    pub(crate) fn split_window(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
        socket_path: &Path,
        environment_overrides: Option<&[String]>,
        command: Option<&[String]>,
        pane_alert_callback: Option<PaneAlertCallback>,
        pane_exit_callback: Option<PaneExitCallback>,
    ) -> Result<SplitWindowResponse, RmuxError> {
        let session_name = split_window_session_name(&target).clone();
        let internal_direction = split_window_internal_direction(direction);
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let (window_index, new_pane_index, _preview_pane_geometry) =
            preview_split(&self.sessions, &target, internal_direction)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let new_pane_id = self.sessions.allocate_pane_id();
        let (session_id, window_id, new_pane_id, new_pane_geometry, requested_cwd) = {
            let session = self
                .sessions
                .session_mut(&session_name)
                .ok_or_else(|| session_not_found(&session_name))?;
            let committed_index = match target {
                SplitWindowTarget::Session(_) => session
                    .split_pane_in_window_with_id_and_direction(
                        session.active_window_index(),
                        session.active_pane_index(),
                        new_pane_id,
                        internal_direction,
                    )?,
                SplitWindowTarget::Pane(target) => session
                    .split_pane_in_window_with_id_and_direction(
                        target.window_index(),
                        target.pane_index(),
                        new_pane_id,
                        internal_direction,
                    )?,
            };
            debug_assert_eq!(committed_index, new_pane_index);
            session
                .window_at(window_index)
                .expect("split target window must exist")
                .pane(committed_index)
                .map(|pane| {
                    (
                        pane.id(),
                        pane_terminal_geometry_for_session(session, &self.options, pane.geometry()),
                    )
                })
                .ok_or_else(|| missing_pane_terminal(&session_name, window_index, committed_index))
                .map(|(pane_id, pane_geometry)| {
                    let window_id = session
                        .window_at(window_index)
                        .expect("split target window must exist")
                        .id();
                    (
                        session.id(),
                        window_id,
                        pane_id,
                        pane_geometry,
                        session.cwd(),
                    )
                })
        }?;
        let profile = TerminalProfile::for_session(
            &self.environment,
            &self.options,
            &session_name,
            session_id.as_u32(),
            socket_path,
            true,
            environment_overrides,
            Some(new_pane_id),
            requested_cwd,
        )?;
        let runtime_window_name = profile.runtime_window_name(command);
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let terminal = open_pane_terminal(
            new_pane_geometry,
            profile,
            runtime_window_name.clone(),
            command,
        )?;
        let pid = terminal.pid();
        let output_reader =
            match clone_terminal_for_output_reader(&terminal, &session_name, new_pane_id) {
                Ok(output_reader) => output_reader,
                Err(error) => {
                    self.replace_session(&session_name, previous_session)?;
                    return Err(error);
                }
            };
        #[cfg(windows)]
        let exit_watcher =
            match clone_terminal_for_exit_watcher(&terminal, &session_name, new_pane_id) {
                Ok(exit_watcher) => exit_watcher,
                Err(error) => {
                    self.replace_session(&session_name, previous_session)?;
                    return Err(error);
                }
            };

        if let Err(error) = self.terminals.insert_pane(
            runtime_session_name.clone(),
            new_pane_id,
            window_index,
            new_pane_index,
            terminal,
        ) {
            self.replace_session(&session_name, previous_session)?;
            return Err(error);
        }
        if let Err(error) = self.insert_pane_output(
            &runtime_session_name,
            new_pane_id,
            PaneOutputSpawn {
                geometry: new_pane_geometry,
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback,
                pane_exit_callback,
            },
        ) {
            let _ = self
                .terminals
                .remove_pane(&runtime_session_name, new_pane_id);
            self.replace_session(&session_name, previous_session)?;
            return Err(error);
        }

        if let Err(error) = self.resize_terminals(&session_name) {
            let rollback_target =
                PaneTarget::with_window(session_name.clone(), window_index, new_pane_index);
            self.remove_pane_output(&runtime_session_name, new_pane_id);
            if self
                .terminals
                .remove_pane(&runtime_session_name, new_pane_id)
                .is_none()
            {
                return Err(RmuxError::Server(format!(
                    "failed to roll back session {session_name} after {error}: missing pane terminal for {rollback_target}"
                )));
            }

            self.restore_session_after_resize_error(&session_name, previous_session, &error)?;
            return Err(error);
        }

        let sessions_to_synchronize = self
            .window_link_slots_for(&session_name, window_index)
            .into_iter()
            .map(|slot| slot.session_name)
            .collect::<Vec<_>>();
        self.synchronize_linked_window_from_slot(&session_name, window_index)?;
        for synchronized_session in sessions_to_synchronize {
            self.synchronize_session_group_from(&synchronized_session)?;
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: new_pane_id,
            command: command.map(<[String]>::to_vec),
            working_directory: Some(lifecycle_cwd),
            private_environment: environment_overrides.map(<[String]>::to_vec),
            dimensions: terminal_size_from_geometry(new_pane_geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, new_pane_id);
        self.update_pane_lifecycle_output_sequence(new_pane_id, output_sequence);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(SplitWindowResponse {
            pane: PaneTarget::with_window(session_name, window_index, new_pane_index),
        })
    }

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
        let (hook_context, pane_id, remove_session, removed_option_targets) = {
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
            let removed_option_targets = if kill_all_except {
                window
                    .panes()
                    .iter()
                    .filter(|pane| pane.index() != target.pane_index())
                    .map(|pane| {
                        PaneTarget::with_window(
                            session_name.clone(),
                            target.window_index(),
                            pane.index(),
                        )
                    })
                    .collect()
            } else {
                vec![target.clone()]
            };
            (
                hook_context,
                pane_id,
                !kill_all_except
                    && previous_session.windows().len() == 1
                    && window.pane_count() == 1,
                removed_option_targets,
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
            return Ok(KilledPaneResult {
                response: KillPaneResponse {
                    target,
                    window_destroyed: true,
                },
                hook_context,
                session_destroyed: true,
                removed_session_id: Some(removed_session.id().as_u32()),
                removed_pane_ids: vec![pane_id],
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
        let removed_outputs =
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
        terminate_removed_terminals(&mut removed_terminals);
        self.remove_pane_lifecycles(committed_outcome.removed_pane_ids());

        self.synchronize_session_group_from(&session_name)?;
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        if committed_outcome.window_destroyed() {
            let _ = self
                .options
                .remove_window(&rmux_proto::WindowTarget::with_window(
                    session_name.clone(),
                    target.window_index(),
                ));
        } else {
            for removed_target in removed_option_targets {
                let _ = self.options.remove_pane(&removed_target);
            }
        }

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
        } = request;
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
                pane_terminal_geometry_for_session(session, &self.options, pane.geometry()),
                session.cwd(),
            )
        };

        let pane_was_alive = self.terminals.pane_is_alive(
            &runtime_session_name,
            pane_id,
            window_index,
            pane_index,
        )?;
        if pane_was_alive && !kill {
            return Err(RmuxError::Server(
                "pane still active; use -k to force respawn".to_owned(),
            ));
        }

        let profile = TerminalProfile::for_session(
            &self.environment,
            &self.options,
            &session_name,
            session_id.as_u32(),
            socket_path,
            true,
            environment.as_deref(),
            Some(pane_id),
            start_directory.as_deref().or(requested_cwd),
        )?;
        let automatic_window_name = profile.automatic_window_name(command.as_deref());
        let runtime_window_name = profile.runtime_window_name(command.as_deref());
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let terminal = open_pane_terminal(
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            command.as_deref(),
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&terminal, &session_name, pane_id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, &session_name, pane_id)?;

        if let Some(pipe) = self.remove_pane_pipe(&runtime_session_name, pane_id) {
            pipe.stop();
        }
        if let Some(mut terminal) = self.terminals.remove_pane(&runtime_session_name, pane_id) {
            terminal.terminate_with_bounded_grace();
            if pane_was_alive {
                on_replaced_active_pane(
                    self,
                    &KilledPaneHookContext {
                        target: target.clone(),
                        pane_id: pane_id.as_u32(),
                        window_id: window_id.as_u32(),
                        window_name,
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
            command,
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

fn terminate_removed_terminals(
    terminals: &mut std::collections::HashMap<PaneId, crate::pane_terminal_process::PaneTerminal>,
) {
    for terminal in terminals.values_mut() {
        terminal.terminate_with_bounded_grace();
    }
}

fn clone_terminal_for_output_reader(
    terminal: &PaneTerminal,
    session_name: &SessionName,
    pane_id: PaneId,
) -> Result<PtyMaster, RmuxError> {
    terminal.clone_master().map_err(|error| {
        RmuxError::Server(format!(
            "failed to clone pane output reader for pane id {} in session {}: {error}",
            pane_id.as_u32(),
            session_name
        ))
    })
}

#[cfg(windows)]
fn clone_terminal_for_exit_watcher(
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
