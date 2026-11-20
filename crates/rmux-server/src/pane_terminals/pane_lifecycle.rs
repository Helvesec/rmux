use std::path::Path;

use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName, TerminalPixels};
use rmux_pty::PtyMaster;

use crate::pane_terminal_lookup::{initial_pane, SessionPane};
use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::{SessionBaseEnvironment, TerminalProfile};

use super::lifecycle_state::terminal_size_from_geometry;
use super::pane_terminal_store::native_resize::{NativeResizePlan, WindowRuntimeResizeError};
use super::{
    session_not_found, HandlerState, InitialPaneSpawnOptions, PaneLifecycleSpawn, PaneOutputSpawn,
    PaneWindowGeometry, WindowNameApplication, WindowRuntimeResizeScope, WindowSpawnOptions,
};

mod kill;

mod respawn;

mod window_terminal;

mod preview;

mod split;

pub(crate) enum SplitPaneProcess<'a> {
    Process(Option<&'a rmux_proto::ProcessCommand>),
    Empty,
}

/// Command semantics are richer than the published SDK boolean. Keep the
/// override internal and install it as part of the split transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SplitExitPolicyOverride {
    Off,
    On,
    Key,
}

impl From<bool> for SplitExitPolicyOverride {
    fn from(keep: bool) -> Self {
        if keep {
            Self::On
        } else {
            Self::Off
        }
    }
}

impl SplitExitPolicyOverride {
    fn option_value(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Key => "key",
        }
    }
}

mod linked_kill;
pub(in crate::pane_terminals) use linked_kill::LinkedWindowTransferRemovalPlan;

impl HandlerState {
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
                PaneWindowGeometry::new(session.name(), pane.window_index, window)
                    .content_geometry(&self.options, pane.index, pane.geometry, false, false),
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
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_window_name = if crate::automatic_rename::automatic_rename_enabled(
            &self.options,
            session_name,
            pane.window_index,
        ) {
            profile.automatic_window_name(spawn.command)
        } else {
            runtime_window_name.clone()
        };
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = open_pane_terminal(
            self.pane_shell.0.as_ref(),
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            spawn.command,
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&mut terminal, session_name, pane.id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, session_name, pane.id)?;

        self.apply_window_name(
            session_name,
            pane.window_index,
            initial_window_name,
            WindowNameApplication::Initial,
        )?;

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
    ) -> Result<NativeResizePlan, WindowRuntimeResizeError> {
        self.resize_window_terminal_runtime_scoped(
            session_name,
            window_index,
            WindowRuntimeResizeScope::Window,
        )
    }

    pub(crate) fn resize_window_terminal_runtime_scoped(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        scope: WindowRuntimeResizeScope,
    ) -> Result<NativeResizePlan, WindowRuntimeResizeError> {
        #[cfg(test)]
        {
            self.window_runtime_resize_count = self.window_runtime_resize_count.saturating_add(1);
        }
        let (runtime_session_name, pane_geometries) =
            self.window_runtime_resize_snapshot_scoped(session_name, window_index, &scope)?;
        let terminal_pixels = self.attached_terminal_pixels.get(session_name).copied();
        let plan = self.native_resize_plan(runtime_session_name, pane_geometries, terminal_pixels);
        let completed = self.terminals.resize_session(plan)?;
        self.resize_transcripts(&completed.runtime_session, &completed.panes);
        Ok(completed)
    }

    pub(crate) fn window_runtime_resize_snapshot(
        &self,
        session_name: &SessionName,
        window_index: u32,
    ) -> Result<Vec<SessionPane>, RmuxError> {
        self.window_runtime_resize_snapshot_scoped(
            session_name,
            window_index,
            &WindowRuntimeResizeScope::Window,
        )
        .map(|(_, panes)| panes)
    }

    fn window_runtime_resize_snapshot_scoped(
        &self,
        session_name: &SessionName,
        window_index: u32,
        scope: &WindowRuntimeResizeScope,
    ) -> Result<(SessionName, Vec<SessionPane>), RmuxError> {
        let (runtime_session_name, pane_geometries) = {
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
                .filter(|pane| match scope {
                    WindowRuntimeResizeScope::Window => true,
                    WindowRuntimeResizeScope::Pane(pane_id) => pane.id() == *pane_id,
                    WindowRuntimeResizeScope::Panes(pane_ids) => pane_ids.contains(&pane.id()),
                })
                .map(|pane| {
                    let (alternate_on, copy_mode_active) =
                        self.resized_pane_viewport_state(session_name, window_index, pane.id());
                    SessionPane {
                        id: pane.id(),
                        window_index,
                        index: pane.index(),
                        geometry: PaneWindowGeometry::new(session.name(), window_index, window)
                            .content_geometry(
                                &self.options,
                                pane.index(),
                                pane.geometry(),
                                alternate_on,
                                copy_mode_active,
                            ),
                        window_size: window.size(),
                    }
                })
                .collect::<Vec<_>>();
            match scope {
                WindowRuntimeResizeScope::Pane(pane_id) if pane_geometries.is_empty() => {
                    return Err(RmuxError::pane_not_found(session_name.clone(), *pane_id));
                }
                WindowRuntimeResizeScope::Panes(pane_ids)
                    if pane_geometries.len() != pane_ids.len() =>
                {
                    return Err(RmuxError::Server(
                        "pane runtime resize scope no longer matches the window".to_owned(),
                    ));
                }
                _ => {}
            }
            (runtime_session_name, pane_geometries)
        };
        Ok((runtime_session_name, pane_geometries))
    }

    pub(crate) fn resize_terminals(&mut self, session_name: &SessionName) -> Result<(), RmuxError> {
        let terminal_pixels = self.attached_terminal_pixels.get(session_name).copied();
        for (runtime_session_name, pane_geometries) in
            self.session_pane_terminal_geometries_by_runtime(session_name)?
        {
            let plan =
                self.native_resize_plan(runtime_session_name, pane_geometries, terminal_pixels);
            let completed = self
                .terminals
                .resize_session(plan)
                .map_err(|failure| RmuxError::from(*failure))?;
            self.resize_transcripts(&completed.runtime_session, &completed.panes);
        }
        self.sync_pane_lifecycle_dimensions_for_session(session_name);
        Ok(())
    }

    fn native_resize_plan(
        &self,
        runtime_session: SessionName,
        panes: Vec<SessionPane>,
        terminal_pixels: Option<TerminalPixels>,
    ) -> NativeResizePlan {
        NativeResizePlan {
            #[cfg(windows)]
            pending_panes: self
                .starting_panes
                .get(&runtime_session)
                .into_iter()
                .flat_map(|pending| pending.keys().copied())
                .filter(|pane_id| {
                    self.pane_lifecycle.get(pane_id).is_some_and(|pane| {
                        pane.process == super::lifecycle_state::PaneLifecycleProcessState::Starting
                    })
                })
                .collect(),
            runtime_session,
            panes,
            terminal_pixels,
        }
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
        let runtime_window_name = profile.runtime_window_name(spawn.command);
        let initial_window_name = if crate::automatic_rename::automatic_rename_enabled(
            &self.options,
            session_name,
            window_index,
        ) {
            profile.automatic_window_name(spawn.command)
        } else {
            runtime_window_name.clone()
        };
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = open_pane_terminal(
            self.pane_shell.0.as_ref(),
            pane_geometry,
            profile,
            runtime_window_name.clone(),
            spawn.command,
        )?;
        let pid = terminal.pid();
        let output_reader = clone_terminal_for_output_reader(&mut terminal, session_name, pane_id)?;
        #[cfg(windows)]
        let exit_watcher = clone_terminal_for_exit_watcher(&terminal, session_name, pane_id)?;

        self.apply_window_name(
            session_name,
            window_index,
            initial_window_name,
            WindowNameApplication::Initial,
        )?;

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
