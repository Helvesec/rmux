use std::collections::HashSet;
use std::path::Path;

use rmux_core::{PaneId, Session};
use rmux_proto::{
    KillPaneResponse, PaneTarget, RespawnPaneRequest, RespawnPaneResponse, RmuxError, SessionName,
};
use rmux_pty::PtyMaster;

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_lookup::{initial_pane, SessionPane};
use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::{validate_process_command, SessionBaseEnvironment, TerminalProfile};

use super::lifecycle_state::terminal_size_from_geometry;
use super::{
    pane_terminal_geometry_for_session, session_not_found, HandlerState, InitialPaneSpawnOptions,
    KilledPaneHookContext, KilledPaneResult, PaneLifecycleSpawn, PaneOutputSpawn,
    SessionTransferSnapshot, WindowSpawnOptions,
};

#[path = "pane_lifecycle/preview.rs"]
mod preview;

#[path = "pane_lifecycle/split.rs"]
mod split;

#[path = "pane_lifecycle/linked_kill.rs"]
mod linked_kill;
pub(in crate::pane_terminals) use linked_kill::LinkedWindowTransferRemovalPlan;

use preview::preview_kill_pane;

#[derive(Clone, Copy, PartialEq, Eq)]
enum GroupedLastPaneAction {
    KillSharedPane,
    RemoveAddressedAlias,
}

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
            pane.index(),
            pane.geometry(),
            false,
            false,
        );
        let mut profile = TerminalProfile::for_session_with_base_environment(
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
        if let Some(shell) = spawn.respawn_shell {
            profile = profile.with_respawn_shell(shell.to_path_buf());
        }
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
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
                process_command: spawn.command.cloned(),
                working_directory: Some(lifecycle_cwd),
                respawn_shell,
                private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
                respawn_environment: spawn.respawn_environment.map(<[String]>::to_vec),
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
        retained_output_sender: Option<crate::pane_io::PaneOutputSender>,
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
        if let Err(error) = self.reset_pane_output_with_sender(
            runtime_session_name,
            pane_id,
            output,
            retained_output_sender,
        ) {
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
                    pane.index,
                    pane.geometry,
                    false,
                    false,
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
        let respawn_shell = profile.shell().to_path_buf();
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
            process_command: spawn.command.cloned(),
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
            respawn_environment: None,
            dimensions: terminal_size_from_geometry(pane.geometry),
            pid: Some(pid),
        });
        let output_sequence = self.pane_output_generation(&runtime_session_name, pane.id);
        self.update_pane_lifecycle_output_sequence(pane.id, output_sequence);

        Ok(())
    }

    pub(crate) fn resize_window_terminal_runtime(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<(), RmuxError> {
        #[cfg(test)]
        {
            self.window_runtime_resize_count = self.window_runtime_resize_count.saturating_add(1);
        }
        let (runtime_session_name, window_size, pane_geometries) = {
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
            let runtime_session_name =
                self.runtime_session_name_for_window(session_name, window_index);
            let pane_geometries = window
                .panes()
                .iter()
                .map(|pane| {
                    let (alternate_on, copy_mode_active) =
                        self.pane_viewport_state(session_name, window_index, pane.id());
                    SessionPane {
                        id: pane.id(),
                        window_index,
                        index: pane.index(),
                        geometry: pane_terminal_geometry_for_session(
                            session,
                            &self.options,
                            window_index,
                            pane.index(),
                            pane.geometry(),
                            alternate_on,
                            copy_mode_active,
                        ),
                    }
                })
                .collect::<Vec<_>>();
            (runtime_session_name, window.size(), pane_geometries)
        };
        let terminal_pixels = self.attached_terminal_pixels.get(session_name).copied();
        self.terminals.resize_session(
            &runtime_session_name,
            &pane_geometries,
            window_size,
            terminal_pixels,
        )?;
        self.resize_transcripts(&runtime_session_name, &pane_geometries);
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
                    pane.index(),
                    pane.geometry(),
                    false,
                    false,
                ),
                session.cwd().map(Path::to_path_buf),
            )
        };
        let captured_base_environment =
            self.session_base_environment_for_window(session_name, window_index);
        let base_environment = base_environment_override.or(captured_base_environment.as_ref());
        let mut profile = TerminalProfile::for_session_with_base_environment(
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
        if let Some(shell) = spawn.respawn_shell {
            profile = profile.with_respawn_shell(shell.to_path_buf());
        }
        let automatic_window_name = profile.automatic_window_name(spawn.command);
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
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
            process_command: spawn.command.cloned(),
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
            respawn_environment: spawn.respawn_environment.map(<[String]>::to_vec),
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
        let session_name = target.session_name().clone();
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
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
                addressed_last_pane && previous_session.windows().len() == 1,
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
        let transfer_snapshot = SessionTransferSnapshot::capture(self);

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
        let mut affected_sessions = if committed_outcome.window_destroyed() {
            vec![session_name.clone()]
        } else {
            let synchronize_result = (|| {
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
                    transfer_snapshot.restore(self);
                    return Err(error);
                }
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
                    transfer_snapshot.restore(self);
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
            transfer_snapshot.restore(self);
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
        for (affected_session, before) in before_pane_options {
            self.rekey_pane_options_after_session_change(&before, &affected_session)?;
        }

        if committed_outcome.window_destroyed() {
            let _ = self.detach_window_link_slot(&session_name, target.window_index());
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
            affected_sessions,
            destroyed_sessions: Vec::new(),
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
                pane_terminal_geometry_for_session(
                    session,
                    &self.options,
                    window_index,
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
    terminal.clone_child_for_exit_teardown().map_err(|error| {
        RmuxError::Server(format!(
            "failed to clone pane exit watcher for pane id {} in session {}: {error}",
            pane_id.as_u32(),
            session_name
        ))
    })
}
