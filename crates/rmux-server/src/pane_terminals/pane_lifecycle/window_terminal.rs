use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};

use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::{SessionBaseEnvironment, TerminalProfile};

use super::super::lifecycle_state::terminal_size_from_geometry;
use super::super::{
    HandlerState, PaneLifecycleSpawn, PaneOutputSpawn, PaneWindowGeometry, WindowSpawnOptions,
};
#[cfg(windows)]
use super::clone_terminal_for_exit_watcher;
use super::clone_terminal_for_output_reader;

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
        session: rmux_core::ManagedSessionView<'_>,
        window_index: u32,
        window: &rmux_core::Window,
        spawn: WindowSpawnOptions<'_>,
        base_environment: Option<&SessionBaseEnvironment>,
    ) -> Result<PreparedWindowTerminal, RmuxError> {
        let pane = window.pane(0).ok_or_else(|| {
            RmuxError::Server(format!(
                "initial pane missing for session {}:{window_index}",
                session.name()
            ))
        })?;
        let pane_geometry = PaneWindowGeometry::new(session.name(), window_index, window)
            .content_geometry(&self.options, pane.index(), pane.geometry(), false, false);
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
        // Window respawn removes the old transcript before installing this
        // terminal. Capture the stable pane's title while it is still present,
        // including when the window is addressed through a linked alias.
        let runtime_session_name =
            self.runtime_session_name_for_window(session.name(), window_index);
        let initial_title = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane.id()))
            .map(|transcript| transcript.lock().title().to_owned())
            .or_else(|| profile.initial_pane_title());
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = open_pane_terminal(
            self.pane_shell.0.as_ref(),
            pane_geometry,
            profile,
            runtime_window_name,
            spawn.command,
        )?;
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
        retained_colours: Option<crate::control_colours::PaneControlColours>,
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
            retained_colours,
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
}
