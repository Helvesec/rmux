//! Handler-state side of `split-window`.
//!
//! Pulled out of `pane_lifecycle.rs` so the parent file stays compact and so
//! the split flow — which is dense with rollback paths — owns its own
//! module. Keeps the same `impl HandlerState` so callers see no API change.

use std::path::{Path, PathBuf};

use rmux_proto::{
    OptionName, PaneTarget, ProcessCommand, RmuxError, ScopeSelector, SetOptionMode,
    SplitDirection, SplitWindowResponse, SplitWindowTarget,
};

use crate::pane_io::{PaneAlertCallback, PaneExitCallback};
use crate::pane_terminal_lookup::missing_pane_terminal;
use crate::pane_terminal_process::open_pane_terminal;
use crate::terminal::{validate_process_command, TerminalProfile};

use super::super::lifecycle_state::terminal_size_from_geometry;
use super::super::{
    pane_terminal_geometry_for_session, session_not_found, HandlerState, PaneLifecycleSpawn,
    PaneOutputSpawn,
};
#[cfg(windows)]
use super::clone_terminal_for_exit_watcher;
use super::clone_terminal_for_output_reader;
use super::preview::{preview_split, split_window_internal_direction, split_window_session_name};

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
        command: Option<&ProcessCommand>,
        start_directory: Option<&Path>,
        keep_alive_on_exit: Option<bool>,
        split_size: Option<u32>,
        full_size: bool,
        detached: bool,
        pane_alert_callback: Option<PaneAlertCallback>,
        pane_exit_callback: Option<PaneExitCallback>,
    ) -> Result<SplitWindowResponse, RmuxError> {
        validate_process_command(command)?;
        let session_name = split_window_session_name(&target).clone();
        let internal_direction = split_window_internal_direction(direction);
        let previous_session = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        let previous_options = self.options.clone();
        let before_pane_options = self.pane_option_slots_for_session(&session_name)?;
        let (window_index, new_pane_index, _preview_pane_geometry) =
            preview_split(&self.sessions, &target, internal_direction, before)?;
        let runtime_session_name =
            self.runtime_session_name_for_window(&session_name, window_index);
        let new_pane_id = self.sessions.allocate_pane_id();
        let (
            session_id,
            window_id,
            new_pane_id,
            committed_pane_index,
            new_pane_geometry,
            requested_cwd,
        ) = self.commit_split_into_session(
            &session_name,
            &target,
            window_index,
            new_pane_id,
            new_pane_index,
            internal_direction,
            before,
            split_size,
            full_size,
            detached,
        )?;
        if let Err(error) =
            self.rekey_pane_options_after_session_change(&before_pane_options, &session_name)
        {
            self.options = previous_options;
            self.replace_session(&session_name, previous_session)?;
            return Err(error);
        }
        let new_target =
            PaneTarget::with_window(session_name.clone(), window_index, committed_pane_index);
        if let Some(keep_alive) = keep_alive_on_exit {
            if let Err(error) = self.options.set(
                ScopeSelector::Pane(new_target.clone()),
                OptionName::RemainOnExit,
                if keep_alive { "on" } else { "off" }.to_owned(),
                SetOptionMode::Replace,
            ) {
                self.options = previous_options;
                self.replace_session(&session_name, previous_session)?;
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
                self.replace_session(&session_name, previous_session)?;
                return Err(error);
            }
        };
        let runtime_window_name = profile.runtime_window_name(command);
        let initial_title = profile.initial_pane_title();
        let lifecycle_cwd = profile.cwd().to_path_buf();
        let respawn_shell = profile.shell().to_path_buf();
        let mut terminal = match open_pane_terminal(
            new_pane_geometry,
            profile,
            runtime_window_name.clone(),
            command,
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                self.options = previous_options;
                self.replace_session(&session_name, previous_session)?;
                return Err(error);
            }
        };
        let pid = terminal.pid();
        let output_reader =
            match clone_terminal_for_output_reader(&mut terminal, &session_name, new_pane_id) {
                Ok(output_reader) => output_reader,
                Err(error) => {
                    self.options = previous_options;
                    self.replace_session(&session_name, previous_session)?;
                    return Err(error);
                }
            };
        #[cfg(windows)]
        let exit_watcher =
            match clone_terminal_for_exit_watcher(&terminal, &session_name, new_pane_id) {
                Ok(exit_watcher) => exit_watcher,
                Err(error) => {
                    self.options = previous_options;
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
            self.options = previous_options;
            self.replace_session(&session_name, previous_session)?;
            return Err(error);
        }
        if let Err(error) = self.insert_pane_output(
            &runtime_session_name,
            new_pane_id,
            PaneOutputSpawn {
                geometry: new_pane_geometry,
                initial_title,
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
            self.options = previous_options;
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

            self.options = previous_options;
            self.restore_session_after_resize_error(&session_name, previous_session, &error)?;
            return Err(error);
        }

        let synchronized_sessions =
            self.synchronize_window_alias_family_from_slot(&session_name, window_index)?;
        let synchronized_source = self
            .sessions
            .session(&session_name)
            .cloned()
            .ok_or_else(|| session_not_found(&session_name))?;
        self.synchronize_pane_alias_options_from_session(&synchronized_source)?;
        for synchronized_session in synchronized_sessions {
            self.sync_pane_lifecycle_dimensions_for_session(&synchronized_session);
        }
        self.record_pane_lifecycle_spawn(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: new_pane_id,
            process_command: command.cloned(),
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: environment_overrides.map(<[String]>::to_vec),
            respawn_environment: None,
            dimensions: terminal_size_from_geometry(new_pane_geometry),
            pid: Some(pid),
        });
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
        new_pane_id: rmux_core::PaneId,
        expected_pane_index: u32,
        direction: SplitDirection,
        before: bool,
        split_size: Option<u32>,
        full_size: bool,
        detached: bool,
    ) -> Result<CommittedSplit, RmuxError> {
        // Capture `cwd` as an owned `PathBuf` so the caller can keep it past
        // the `&mut SessionStore` borrow.
        let session = self
            .sessions
            .session_mut(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let (target_window_index, target_pane_index) = match target {
            SplitWindowTarget::Session(_) => {
                (session.active_window_index(), session.active_pane_index())
            }
            SplitWindowTarget::Pane(pane) => (pane.window_index(), pane.pane_index()),
        };
        let committed_index = if full_size {
            session.split_pane_full_size_in_window_with_id_and_direction_before_detached(
                target_window_index,
                target_pane_index,
                new_pane_id,
                direction,
                before,
                detached,
            )?
        } else {
            let committed_index = session
                .split_pane_in_window_with_id_and_direction_before_detached(
                    target_window_index,
                    target_pane_index,
                    new_pane_id,
                    direction,
                    before,
                    detached,
                )?;
            debug_assert_eq!(committed_index, expected_pane_index);
            committed_index
        };
        if let Some(split_size) = split_size {
            session.resize_new_split_pane_to_in_window(
                target_window_index,
                committed_index,
                direction,
                split_size,
                before,
            )?;
        }
        let pane = session
            .window_at(window_index)
            .expect("split target window must exist")
            .pane(committed_index)
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, committed_index))?;
        let pane_id = pane.id();
        let pane_geometry = pane_terminal_geometry_for_session(
            session,
            &self.options,
            window_index,
            pane.geometry(),
        );
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
