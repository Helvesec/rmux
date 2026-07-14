use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};

use crate::pane_io::PaneExitEvent;
use crate::pane_terminal_lookup::initial_pane;
use crate::pane_terminal_process::{open_pane_terminal, pty_geometry_from_layout, PaneTerminal};

use super::lifecycle_state::terminal_size_from_geometry;
use super::pane_lifecycle::{clone_terminal_for_exit_watcher, clone_terminal_for_output_reader};
use super::{
    pane_terminal_geometry_for_session, session_not_found, CompletedDeferredInitialPane,
    DeferredInitialPaneConsoleInputAction, DeferredInitialPaneInput, DeferredInitialPaneInputFlush,
    DeferredInitialPaneSpawn, HandlerState, InitialPaneSpawnOptions, PaneExitMetadata,
    PaneLifecycleSpawn, PaneOutputSpawn, StartingPane,
};

const STARTING_PANE_INPUT_MAX_BYTES: usize = 64 * 1024;

impl HandlerState {
    pub(crate) fn prepare_deferred_initial_session_terminal(
        &mut self,
        session_name: &SessionName,
        spawn: InitialPaneSpawnOptions<'_>,
    ) -> Result<DeferredInitialPaneSpawn, RmuxError> {
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
        let profile = crate::terminal::TerminalProfile::for_initial_session_pane(
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
        self.apply_automatic_window_name(session_name, pane.window_index, automatic_window_name)?;
        self.terminals.insert_pending_session(
            runtime_session_name.clone(),
            crate::terminal::SessionBaseEnvironment::from_profile(&profile),
        )?;
        self.record_pane_lifecycle_starting(PaneLifecycleSpawn {
            session_id,
            window_id,
            pane_id: pane.id,
            process_command: spawn.command.cloned(),
            working_directory: Some(lifecycle_cwd),
            respawn_shell,
            private_environment: spawn.environment_overrides.map(<[String]>::to_vec),
            respawn_environment: None,
            dimensions: terminal_size_from_geometry(pane.geometry),
            pid: None,
        });
        let generation = self.insert_pending_pane_output(
            &runtime_session_name,
            pane.id,
            pane_geometry,
            initial_title,
        )?;
        self.starting_panes
            .entry(runtime_session_name.clone())
            .or_default()
            .insert(
                pane.id,
                StartingPane {
                    profile: profile.clone(),
                    runtime_window_name: runtime_window_name.clone(),
                    generation,
                    queued_input: Default::default(),
                    queued_input_bytes: 0,
                },
            );

        Ok(DeferredInitialPaneSpawn {
            runtime_session_name,
            visible_session_name: session_name.clone(),
            pane_id: pane.id,
            geometry: pane_geometry,
            profile,
            runtime_window_name,
            command: spawn.command.cloned(),
            generation,
            pane_alert_callback: spawn.pane_alert_callback,
            pane_exit_callback: spawn.pane_exit_callback,
        })
    }

    pub(crate) fn open_deferred_initial_pane_terminal(
        job: &DeferredInitialPaneSpawn,
    ) -> Result<PaneTerminal, RmuxError> {
        open_pane_terminal(
            job.geometry,
            job.profile.clone(),
            job.runtime_window_name.clone(),
            job.command.as_ref(),
        )
    }

    pub(crate) fn complete_deferred_initial_pane_spawn(
        &mut self,
        job: DeferredInitialPaneSpawn,
        mut terminal: PaneTerminal,
    ) -> Result<Option<CompletedDeferredInitialPane>, RmuxError> {
        let Some(runtime_session_name) = self.starting_runtime_session_for_job(&job) else {
            return Ok(None);
        };
        let Some(target) = self.pane_target_for_runtime_pane(&runtime_session_name, job.pane_id)
        else {
            self.remove_starting_pane(&runtime_session_name, job.pane_id);
            return Ok(None);
        };
        let (pane_geometry, session_size, terminal_pixels) = {
            let session = self
                .sessions
                .session(target.session_name())
                .ok_or_else(|| session_not_found(target.session_name()))?;
            let window = session.window_at(target.window_index()).ok_or_else(|| {
                RmuxError::invalid_target(
                    format!("{}:{}", target.session_name(), target.window_index()),
                    "window index does not exist in session",
                )
            })?;
            let pane = window.pane(target.pane_index()).ok_or_else(|| {
                RmuxError::invalid_target(target.to_string(), "pane index does not exist in window")
            })?;
            (
                pane_terminal_geometry_for_session(
                    session,
                    &self.options,
                    target.window_index(),
                    pane.geometry(),
                ),
                window.size(),
                self.attached_terminal_pixels
                    .get(target.session_name())
                    .copied(),
            )
        };
        terminal
            .resize(pty_geometry_from_layout(
                pane_geometry,
                session_size,
                terminal_pixels,
            ))
            .map_err(|error| {
                RmuxError::Server(format!(
                    "failed to resize deferred pane terminal for {target}: {error}"
                ))
            })?;
        let pid = terminal.pid();
        let output_reader =
            clone_terminal_for_output_reader(&mut terminal, target.session_name(), job.pane_id)?;
        let exit_watcher =
            clone_terminal_for_exit_watcher(&terminal, target.session_name(), job.pane_id)?;
        let (queued_input, input_writer) = {
            let starting = self
                .starting_panes
                .get_mut(&runtime_session_name)
                .and_then(|panes| panes.get_mut(&job.pane_id))
                .ok_or_else(|| {
                    RmuxError::Server(format!(
                        "missing starting pane state for pane id {} in session {}",
                        job.pane_id.as_u32(),
                        runtime_session_name
                    ))
                })?;
            let queued_input = starting.queued_input.drain(..).collect::<Vec<_>>();
            starting.queued_input_bytes = 0;
            let input_writer = if queued_input.is_empty() {
                None
            } else {
                Some(terminal.clone_master().map_err(|error| {
                    RmuxError::Server(format!(
                        "failed to clone deferred pane input writer for {target}: {error}"
                    ))
                })?)
            };
            (queued_input, input_writer)
        };

        if self.terminals.contains_session(&runtime_session_name) {
            self.terminals.insert_pane(
                runtime_session_name.clone(),
                job.pane_id,
                target.window_index(),
                target.pane_index(),
                terminal,
            )?;
        } else {
            self.terminals
                .insert_session(runtime_session_name.clone(), job.pane_id, terminal)?;
        }
        let output_sequence = self.activate_pending_pane_output(
            &runtime_session_name,
            job.pane_id,
            PaneOutputSpawn {
                geometry: pane_geometry,
                initial_title: None,
                output_reader,
                exit_watcher: Some(exit_watcher),
                pane_alert_callback: job.pane_alert_callback,
                pane_exit_callback: job.pane_exit_callback,
            },
        )?;
        self.mark_pane_lifecycle_running(job.pane_id, Some(pid));
        self.update_pane_lifecycle_output_sequence(job.pane_id, output_sequence);
        self.sync_pane_lifecycle_dimensions_for_session(target.session_name());

        Ok(Some(CompletedDeferredInitialPane {
            visible_session_name: target.session_name().clone(),
            runtime_session_name,
            pane_id: job.pane_id,
            pane_pid: pid,
            input_writer,
            queued_input,
        }))
    }

    pub(crate) fn take_deferred_initial_pane_input_or_finish(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<DeferredInitialPaneInputFlush>, RmuxError> {
        let Some(starting) = self
            .starting_panes
            .get_mut(runtime_session_name)
            .and_then(|panes| panes.get_mut(&pane_id))
        else {
            return Ok(None);
        };

        if starting.queued_input.is_empty() {
            // Finish under the same state lock used to observe the empty
            // queue. A writer can now either enqueue before this removal and
            // be drained, or observe no StartingPane and write to the live
            // terminal; it can never enqueue into an entry removed later.
            let _ = self.remove_starting_pane(runtime_session_name, pane_id);
            return Ok(None);
        }

        let queued_input = starting.queued_input.drain(..).collect::<Vec<_>>();
        starting.queued_input_bytes = 0;
        let target = self.pane_target_for_runtime_pane(runtime_session_name, pane_id);
        let Some(target) = target else {
            let _ = self.remove_starting_pane(runtime_session_name, pane_id);
            return Ok(None);
        };
        let input_writer = self.terminals.clone_pane_master(
            runtime_session_name,
            pane_id,
            target.window_index(),
            target.pane_index(),
        )?;
        let pane_pid = self.terminals.pane_pid(
            runtime_session_name,
            pane_id,
            target.window_index(),
            target.pane_index(),
        )?;

        Ok(Some(DeferredInitialPaneInputFlush {
            input_writer,
            pane_pid,
            queued_input,
        }))
    }

    pub(crate) fn finish_deferred_initial_pane_input_after_error(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) {
        let _ = self.remove_starting_pane(runtime_session_name, pane_id);
    }

    pub(crate) fn cancel_starting_pane(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> bool {
        self.remove_starting_pane(runtime_session_name, pane_id)
            .is_some()
    }

    pub(crate) fn fail_deferred_initial_pane_spawn(
        &mut self,
        job: &DeferredInitialPaneSpawn,
        error: &RmuxError,
    ) -> Option<PaneExitEvent> {
        let runtime_session_name = self.starting_runtime_session_for_job(job)?;
        let visible_session_name = self
            .pane_target_for_runtime_pane(&runtime_session_name, job.pane_id)
            .map(|target| target.session_name().clone())
            .unwrap_or_else(|| job.visible_session_name.clone());
        let _ = self.remove_starting_pane(&runtime_session_name, job.pane_id);
        let message = format!(
            "failed to spawn pane {} in session {}: {error}",
            job.pane_id.as_u32(),
            visible_session_name
        );
        self.add_message(message.clone());
        let bytes = format!("{message}\r\n").into_bytes();
        let _ = self.append_bytes_to_runtime_pane_transcript(
            &runtime_session_name,
            job.pane_id,
            &bytes,
        );
        if let Some(sender) = self
            .pane_outputs
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&job.pane_id))
        {
            let _ = sender.send_for_generation(Some(job.generation), bytes);
            let _ = sender.send_for_generation(Some(job.generation), Vec::new());
        }
        let metadata = PaneExitMetadata {
            status: Some(1),
            signal: None,
            time: Some(chrono::Local::now().timestamp()),
        };
        self.dead_panes
            .entry(runtime_session_name.clone())
            .or_default()
            .insert(job.pane_id, metadata);
        self.mark_pane_lifecycle_exited(job.pane_id, metadata);
        Some(PaneExitEvent::eof_published(
            runtime_session_name,
            job.pane_id,
            Some(job.generation),
        ))
    }

    pub(crate) fn pane_is_starting_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> bool {
        self.starting_pane_in_window(session_name, window_index, pane_index)
            .is_some()
    }

    pub(crate) fn active_pane_is_starting(&self, session_name: &SessionName) -> bool {
        let Some(session) = self.sessions.session(session_name) else {
            return false;
        };
        self.pane_is_starting_in_window(
            session_name,
            session.active_window_index(),
            session.active_pane_index(),
        )
    }

    pub(crate) fn starting_pane_profile_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Option<&crate::terminal::TerminalProfile> {
        self.starting_pane_in_window(session_name, window_index, pane_index)
            .map(|pane| &pane.profile)
    }

    pub(crate) fn starting_pane_runtime_window_name_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Option<&str> {
        self.starting_pane_in_window(session_name, window_index, pane_index)
            .and_then(|pane| pane.runtime_window_name.as_deref())
    }

    pub(crate) fn starting_pane_base_environment(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::terminal::SessionBaseEnvironment> {
        self.starting_panes
            .get(runtime_session_name)?
            .get(&pane_id)
            .map(|pane| crate::terminal::SessionBaseEnvironment::from_profile(&pane.profile))
    }

    pub(crate) fn queue_starting_pane_input(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        bytes: &[u8],
    ) -> Result<bool, RmuxError> {
        if bytes.is_empty() {
            return Ok(false);
        }
        self.queue_starting_pane_input_entry(
            session_name,
            window_index,
            pane_index,
            DeferredInitialPaneInput::Bytes(bytes.to_vec()),
        )
    }

    pub(crate) fn queue_starting_pane_console_input(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        action: DeferredInitialPaneConsoleInputAction,
        byte_len: usize,
    ) -> Result<bool, RmuxError> {
        self.queue_starting_pane_input_entry(
            session_name,
            window_index,
            pane_index,
            DeferredInitialPaneInput::Console {
                action,
                byte_len: byte_len.max(1),
            },
        )
    }

    fn queue_starting_pane_input_entry(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        input: DeferredInitialPaneInput,
    ) -> Result<bool, RmuxError> {
        let Some(pane_id) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .and_then(|window| window.pane(pane_index))
            .map(rmux_core::Pane::id)
        else {
            return Ok(false);
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(starting) = self
            .starting_panes
            .get_mut(&runtime_session_name)
            .and_then(|panes| panes.get_mut(&pane_id))
        else {
            return Ok(false);
        };
        let next_len = starting.queued_input_bytes.saturating_add(input.byte_len());
        if next_len > STARTING_PANE_INPUT_MAX_BYTES {
            return Err(RmuxError::Server(format!(
                "pane {}:{window_index}.{pane_index} is still starting and its input queue is full",
                session_name
            )));
        }
        starting.queued_input.push_back(input);
        starting.queued_input_bytes = next_len;
        Ok(true)
    }

    fn starting_pane_in_window(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Option<&StartingPane> {
        let pane_id = self
            .sessions
            .session(session_name)?
            .window_at(window_index)?
            .pane(pane_index)?
            .id();
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.starting_panes
            .get(&runtime_session_name)?
            .get(&pane_id)
    }

    fn starting_generation_matches(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
        generation: u64,
    ) -> bool {
        self.starting_panes
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .is_some_and(|pane| pane.generation == generation)
    }

    fn starting_runtime_session_for_job(
        &self,
        job: &DeferredInitialPaneSpawn,
    ) -> Option<SessionName> {
        if self.starting_generation_matches(&job.runtime_session_name, job.pane_id, job.generation)
        {
            return Some(job.runtime_session_name.clone());
        }
        self.starting_panes
            .iter()
            .find(|(_, panes)| {
                panes
                    .get(&job.pane_id)
                    .is_some_and(|pane| pane.generation == job.generation)
            })
            .map(|(session_name, _)| session_name.clone())
    }

    fn remove_starting_pane(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<StartingPane> {
        let panes = self.starting_panes.get_mut(runtime_session_name)?;
        let removed = panes.remove(&pane_id);
        if panes.is_empty() {
            let _ = self.starting_panes.remove(runtime_session_name);
        }
        removed
    }
}
