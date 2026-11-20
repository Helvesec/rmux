//! Handler-state side of `split-window`.
//!
//! Pulled out of `pane_lifecycle.rs` so the parent file stays compact and so
//! the split flow — which is dense with rollback paths — owns its own
//! module. Keeps the same `impl HandlerState` so callers see no API change.

use std::path::{Path, PathBuf};

use rmux_proto::{
    OptionName, PaneTarget, RmuxError, ScopeSelector, SetOptionMode, SplitDirection,
    SplitWindowResponse, SplitWindowTarget,
};

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminal_process::{open_pane_terminal, PaneTerminal};
use crate::terminal::{validate_process_command, TerminalProfile};

use super::super::lifecycle_state::terminal_size_from_geometry;
use super::super::{
    session_not_found, HandlerState, PaneLifecycleSpawn, PaneOutputSpawn, PaneWindowGeometry,
};
#[cfg(windows)]
use super::clone_terminal_for_exit_watcher;
use super::clone_terminal_for_output_reader;
use super::preview::{preview_split, split_window_internal_direction, split_window_session_name};
use super::{SplitExitPolicyOverride, SplitPaneProcess};
#[cfg(all(test, unix))]
#[path = "floating_install_tests.rs"]
mod floating_install_tests;

type CommittedSplit = (
    rmux_core::SessionId,
    rmux_core::WindowId,
    rmux_core::PaneId,
    u32,
    rmux_core::PaneGeometry,
    Option<PathBuf>,
);

impl HandlerState {
    /// Splits the addressed pane, spawning a new pane terminal.
    ///
    /// `before` controls whether the new pane is inserted before (tmux `-b`)
    /// or after the target on the chosen axis.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_window(
        &mut self,
        target: SplitWindowTarget,
        direction: SplitDirection,
        before: bool,
        socket_path: &Path,
        spawn_environment: Option<&std::collections::HashMap<String, String>>,
        environment_overrides: Option<&[String]>,
        process: SplitPaneProcess<'_>,
        start_directory: Option<&Path>,
        keep_alive_on_exit: Option<SplitExitPolicyOverride>,
        split_size: Option<u32>,
        full_size: bool,
        detached: bool,
        floating: Option<rmux_core::FloatingPaneRequest>,
        pane_alert_callback: Option<PaneAlertCallback>,
        pane_exit_callback: Option<PaneExitCallback>,
    ) -> Result<SplitWindowResponse, RmuxError> {
        let empty_pane = matches!(process, SplitPaneProcess::Empty);
        let command = match process {
            SplitPaneProcess::Empty => None,
            SplitPaneProcess::Process(command) => command,
        };
        validate_process_command(command)?;
        let session_name = split_window_session_name(&target).clone();
        self.ensure_session_panes_available(&session_name)?;
        let internal_direction = split_window_internal_direction(direction);
        let previous_options = self.options.clone();
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let (window_index, new_pane_index, _preview_pane_geometry) = match (&target, floating) {
            (SplitWindowTarget::Pane(target), Some(_)) => (
                target.window_index(),
                0,
                rmux_core::PaneGeometry::new(0, 0, 0, 0),
            ),
            _ => preview_split(&self.sessions, &target, internal_direction, before)?,
        };
        let window_target =
            rmux_proto::WindowTarget::with_window(session_name.clone(), window_index);
        let previous_window = self.sessions.checkpoint_window(&window_target)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let (
            session_id,
            window_id,
            new_pane_id,
            committed_pane_index,
            new_pane_geometry,
            requested_cwd,
        ) = match self.commit_split_into_session(
            &session_name,
            &target,
            window_index,
            new_pane_index,
            internal_direction,
            before,
            split_size,
            full_size,
            detached,
            floating,
        ) {
            Ok(committed) => committed,
            Err(error) => {
                self.sessions
                    .restore_window_checkpoint(&window_target, previous_window)?;
                return Err(error);
            }
        };
        let new_pane_index = committed_pane_index;
        if let Err(error) =
            self.rekey_pane_options_after_session_change(&before_pane_options, &session_name)
        {
            self.options = previous_options;
            self.sessions
                .restore_window_checkpoint(&window_target, previous_window)?;
            return Err(error);
        }
        let new_target =
            PaneTarget::with_window(session_name.clone(), window_index, committed_pane_index);
        if let Some(keep_alive) = keep_alive_on_exit {
            if let Err(error) = self.options.set(
                ScopeSelector::Pane(new_target.clone()),
                OptionName::RemainOnExit,
                keep_alive.option_value().to_owned(),
                SetOptionMode::Replace,
            ) {
                self.options = previous_options;
                self.sessions
                    .restore_window_checkpoint(&window_target, previous_window)?;
                return Err(error);
            }
        }

        let base_environment = match &target {
            SplitWindowTarget::Session(session_name) => {
                self.session_base_environment_for_active_pane(session_name)
            }
            SplitWindowTarget::Pane(target) => {
                self.session_base_environment_for_pane_target(target)
            }
        };
        let profile = match TerminalProfile::for_session_with_base_environment(
            &self.environment,
            &self.options,
            &session_name,
            session_id.as_u32(),
            socket_path,
            base_environment.as_ref(),
            spawn_environment,
            true,
            environment_overrides,
            Some(new_pane_id),
            start_directory.or(requested_cwd.as_deref()),
        ) {
            Ok(profile) => profile,
            Err(error) => {
                self.options = previous_options;
                self.sessions
                    .restore_window_checkpoint(&window_target, previous_window)?;
                return Err(error);
            }
        };
        let runtime_window_name = profile.runtime_window_name(command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = match if empty_pane {
            Ok(PaneTerminal::empty(profile))
        } else {
            open_pane_terminal(
                self.pane_shell.0.as_ref(),
                new_pane_geometry,
                profile,
                runtime_window_name.clone(),
                command,
            )
        } {
            Ok(terminal) => terminal,
            Err(error) => {
                self.options = previous_options;
                self.sessions
                    .restore_window_checkpoint(&window_target, previous_window)?;
                return Err(error);
            }
        };
        let pid = terminal.pid();
        let output_spawn = if empty_pane {
            None
        } else {
            let output_reader =
                match clone_terminal_for_output_reader(&mut terminal, &session_name, new_pane_id) {
                    Ok(output_reader) => output_reader,
                    Err(error) => {
                        self.options = previous_options;
                        self.sessions
                            .restore_window_checkpoint(&window_target, previous_window)?;
                        return Err(error);
                    }
                };
            #[cfg(windows)]
            let exit_watcher =
                match clone_terminal_for_exit_watcher(&terminal, &session_name, new_pane_id) {
                    Ok(exit_watcher) => exit_watcher,
                    Err(error) => {
                        self.options = previous_options;
                        self.sessions
                            .restore_window_checkpoint(&window_target, previous_window)?;
                        return Err(error);
                    }
                };

            Some(PaneOutputSpawn {
                geometry: new_pane_geometry,
                initial_title: initial_title.clone(),
                output_reader,
                #[cfg(windows)]
                exit_watcher: Some(exit_watcher),
                pane_alert_callback,
                pane_exit_callback,
            })
        };

        if let Err(error) = self.terminals.insert_pane(
            runtime_session_name.clone(),
            new_pane_id,
            window_index,
            new_pane_index,
            terminal,
        ) {
            self.options = previous_options;
            self.sessions
                .restore_window_checkpoint(&window_target, previous_window)?;
            return Err(error);
        }
        let output_result = match output_spawn {
            Some(spawn) => self.insert_pane_output(&runtime_session_name, new_pane_id, spawn),
            None => self
                .insert_pane_output_state(
                    &runtime_session_name,
                    new_pane_id,
                    new_pane_geometry,
                    initial_title,
                )
                .map(|_| ()),
        };
        if let Err(error) = output_result {
            let _ = self
                .terminals
                .remove_pane(&runtime_session_name, new_pane_id);
            self.options = previous_options;
            self.sessions
                .restore_window_checkpoint(&window_target, previous_window)?;
            return Err(error);
        }

        if let Err(error) = if floating.is_none() {
            self.resize_terminals(&session_name)
        } else {
            Ok(())
        } {
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

            self.options = previous_options;
            self.sessions
                .restore_window_checkpoint(&window_target, previous_window)?;
            self.resize_terminals(&session_name).map_err(|rollback_error| RmuxError::Server(
                format!("failed to roll back session {session_name} after {error}: {rollback_error}")
            ))?;
            return Err(error);
        }

        let synchronized_sessions =
            self.synchronize_window_alias_family_from_slot(&session_name, window_index)?;
        self.synchronize_pane_alias_options_from_name(&session_name)?;
        for synchronized_session in synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(&synchronized_session);
        }
        let lifecycle = PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: new_pane_id,
            process_command: command.cloned(),
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: environment_overrides.map(<[String]>::to_vec),
            respawn_environment: None,
            dimensions: terminal_size_from_geometry(new_pane_geometry),
            pid: (!empty_pane).then_some(pid),
        };
        if empty_pane {
            self.record_pane_lifecycle_empty(lifecycle);
        } else {
            self.record_pane_lifecycle_spawn(lifecycle);
        }
        let output_sequence = self.pane_output_generation(&runtime_session_name, new_pane_id);
        self.update_pane_lifecycle_output_sequence(new_pane_id, output_sequence);
        self.sync_pane_lifecycle_dimensions_for_session(&session_name);

        Ok(SplitWindowResponse { pane: new_target })
    }

    /// Applies the split to the real session store and returns ids + geometry
    /// for the freshly committed pane. Isolating this from the surrounding
    /// terminal-spawn logic keeps `split_window` readable.
    #[allow(clippy::too_many_arguments)]
    fn commit_split_into_session(
        &mut self,
        session_name: &rmux_proto::SessionName,
        target: &SplitWindowTarget,
        window_index: u32,
        expected_pane_index: u32,
        direction: SplitDirection,
        before: bool,
        split_size: Option<u32>,
        full_size: bool,
        detached: bool,
        floating: Option<rmux_core::FloatingPaneRequest>,
    ) -> Result<CommittedSplit, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let (target_window_index, target_pane_index) = match target {
            SplitWindowTarget::Session(_) => {
                (session.active_window_index(), session.active_pane_index())
            }
            SplitWindowTarget::Pane(pane) => (pane.window_index(), pane.pane_index()),
        };
        let window_target =
            rmux_proto::WindowTarget::with_window(session_name.clone(), target_window_index);
        let creation = if let Some(request) = floating {
            self.sessions
                .new_floating_pane(&window_target, request, detached)?
        } else {
            self.sessions.split_pane(
                &window_target,
                target_pane_index,
                rmux_core::ManagedPaneSplit {
                    direction,
                    before,
                    detached,
                    full_size,
                },
            )?
        };
        let committed_index = creation.pane_index;
        if !full_size && floating.is_none() {
            debug_assert_eq!(committed_index, expected_pane_index);
        }
        if let Some(split_size) = split_size {
            self.sessions.resize_new_split_pane_to(
                &window_target,
                committed_index,
                direction,
                split_size,
                before,
            )?;
        }
        let session = self
            .sessions
            .session(session_name)
            .expect("split session exists");
        let pane = session
            .window_at(window_index)
            .expect("split target window must exist")
            .pane(committed_index)
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, committed_index))?;
        let pane_id = pane.id();
        let pane_geometry = PaneWindowGeometry::new(
            session.name(),
            window_index,
            session
                .window_at(window_index)
                .expect("split target window must exist"),
        )
        .content_geometry(&self.options, pane.index(), pane.geometry(), false, false);
        let window_id = session
            .window_at(window_index)
            .expect("split target window must exist")
            .id();
        Ok((
            session.id(),
            window_id,
            pane_id,
            committed_index,
            pane_geometry,
            session.cwd().map(std::path::Path::to_path_buf),
        ))
    }
}
