use rmux_core::{PaneGeometry, PaneId, Utf8Config};
use rmux_proto::{RmuxError, SessionName, TerminalSize};
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::PtyMaster;

#[cfg(windows)]
use crate::pane_io::spawn_pane_exit_watcher;
use crate::pane_io::{
    pane_output_channel, spawn_pane_output_reader, PaneAlertCallback, PaneExitCallback,
    PaneOutputSender,
};
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};

pub(in crate::pane_terminals) struct PaneOutputSpawn {
    pub(in crate::pane_terminals) geometry: PaneGeometry,
    pub(in crate::pane_terminals) initial_title: Option<String>,
    pub(in crate::pane_terminals) output_reader: PtyMaster,
    #[cfg(windows)]
    pub(in crate::pane_terminals) exit_watcher: Option<PtyChild>,
    pub(in crate::pane_terminals) pane_alert_callback: Option<PaneAlertCallback>,
    pub(in crate::pane_terminals) pane_exit_callback: Option<PaneExitCallback>,
}

impl HandlerState {
    #[cfg(windows)]
    pub(in crate::pane_terminals) fn insert_pending_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        geometry: PaneGeometry,
        initial_title: Option<String>,
    ) -> Result<u64, RmuxError> {
        let transcript = PaneTranscript::shared(
            self.history_limit_for_session(session_name),
            TerminalSize {
                cols: geometry.cols(),
                rows: geometry.rows(),
            },
        );
        {
            let mut transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            transcript.set_utf8_config(Utf8Config::from_options(&self.options));
            transcript.set_input_buffer_limit(self.input_buffer_limit());
            transcript.set_alternate_screen_enabled(
                self.alternate_screen_enabled_for_pane_id(session_name, pane_id),
            );
            transcript.set_title_rename_enabled(
                self.title_rename_enabled_for_pane_id(session_name, pane_id),
            );
        }
        seed_initial_pane_title(&transcript, initial_title.as_deref());
        let pane_output = pane_output_channel();

        if self
            .transcripts
            .get(session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Err(RmuxError::Server(format!(
                "pane transcript already exists for pane id {} in session {}",
                pane_id.as_u32(),
                session_name
            )));
        }

        if self
            .pane_outputs
            .get(session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Err(RmuxError::Server(format!(
                "pane output channel already exists for pane id {} in session {}",
                pane_id.as_u32(),
                session_name
            )));
        }

        self.transcripts
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, transcript);
        self.pane_outputs
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, pane_output.clone());
        let generation = self.advance_pane_output_generation(session_name, pane_id);
        pane_output.set_generation(generation);
        if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.update_pane_lifecycle_output_sequence(pane_id, generation);
        self.clear_attached_submitted_line(session_name, pane_id);
        Ok(generation)
    }

    #[cfg(windows)]
    pub(in crate::pane_terminals) fn activate_pending_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        spawn: PaneOutputSpawn,
    ) -> Result<u64, RmuxError> {
        let transcript = self
            .transcripts
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pending pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    session_name
                ))
            })?;
        let pane_output = self
            .pane_outputs
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pending pane output channel for pane id {} in session {}",
                    pane_id.as_u32(),
                    session_name
                ))
            })?;
        let generation = self.pane_output_generation(session_name, pane_id);
        pane_output.set_generation(generation);
        #[cfg(windows)]
        let eof_state = crate::pane_io::PaneOutputEofState::default();
        #[cfg(windows)]
        if let Some(exit_watcher) = spawn.exit_watcher {
            spawn_pane_exit_watcher(
                session_name.clone(),
                pane_id,
                exit_watcher,
                Some(generation),
                eof_state.clone(),
                spawn.pane_exit_callback.clone(),
            );
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        #[cfg(unix)]
        let reader_runtime = self.pane_reader_runtime()?;
        #[cfg(unix)]
        let output_reader_task = spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
            reader_runtime,
        );
        #[cfg(windows)]
        spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            eof_state,
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
        );
        #[cfg(unix)]
        self.pane_output_readers
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, output_reader_task);
        Ok(generation)
    }

    pub(in crate::pane_terminals) fn insert_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        spawn: PaneOutputSpawn,
    ) -> Result<(), RmuxError> {
        let transcript = PaneTranscript::shared(
            self.history_limit_for_session(session_name),
            TerminalSize {
                cols: spawn.geometry.cols(),
                rows: spawn.geometry.rows(),
            },
        );
        {
            let mut transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            transcript.set_utf8_config(Utf8Config::from_options(&self.options));
            transcript.set_input_buffer_limit(self.input_buffer_limit());
            transcript.set_alternate_screen_enabled(
                self.alternate_screen_enabled_for_pane_id(session_name, pane_id),
            );
            transcript.set_title_rename_enabled(
                self.title_rename_enabled_for_pane_id(session_name, pane_id),
            );
        }
        seed_initial_pane_title(&transcript, spawn.initial_title.as_deref());
        let pane_output = pane_output_channel();

        if self
            .transcripts
            .get(session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Err(RmuxError::Server(format!(
                "pane transcript already exists for pane id {} in session {}",
                pane_id.as_u32(),
                session_name
            )));
        }

        if self
            .pane_outputs
            .get(session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Err(RmuxError::Server(format!(
                "pane output channel already exists for pane id {} in session {}",
                pane_id.as_u32(),
                session_name
            )));
        }

        #[cfg(unix)]
        let reader_runtime = self.pane_reader_runtime()?;
        self.transcripts
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, transcript.clone());
        self.pane_outputs
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, pane_output.clone());
        let generation = self.advance_pane_output_generation(session_name, pane_id);
        pane_output.set_generation(generation);
        if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.update_pane_lifecycle_output_sequence(pane_id, generation);
        #[cfg(windows)]
        let eof_state = crate::pane_io::PaneOutputEofState::default();
        #[cfg(windows)]
        if let Some(exit_watcher) = spawn.exit_watcher {
            spawn_pane_exit_watcher(
                session_name.clone(),
                pane_id,
                exit_watcher,
                Some(generation),
                eof_state.clone(),
                spawn.pane_exit_callback.clone(),
            );
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        #[cfg(unix)]
        let output_reader_task = spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
            reader_runtime,
        );
        #[cfg(windows)]
        spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            eof_state,
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
        );
        #[cfg(unix)]
        self.pane_output_readers
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, output_reader_task);
        Ok(())
    }

    pub(in crate::pane_terminals) fn reset_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        spawn: PaneOutputSpawn,
    ) -> Result<(), RmuxError> {
        self.reset_pane_output_with_sender(session_name, pane_id, spawn, None)
    }

    pub(in crate::pane_terminals) fn reset_pane_output_with_sender(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        spawn: PaneOutputSpawn,
        retained_sender: Option<PaneOutputSender>,
    ) -> Result<(), RmuxError> {
        let transcript = PaneTranscript::shared(
            self.history_limit_for_session(session_name),
            TerminalSize {
                cols: spawn.geometry.cols(),
                rows: spawn.geometry.rows(),
            },
        );
        {
            let mut transcript = transcript
                .lock()
                .expect("pane transcript mutex must not be poisoned");
            transcript.set_utf8_config(Utf8Config::from_options(&self.options));
            transcript.set_input_buffer_limit(self.input_buffer_limit());
            transcript.set_alternate_screen_enabled(
                self.alternate_screen_enabled_for_pane_id(session_name, pane_id),
            );
            transcript.set_title_rename_enabled(
                self.title_rename_enabled_for_pane_id(session_name, pane_id),
            );
        }
        seed_initial_pane_title(&transcript, spawn.initial_title.as_deref());
        #[cfg(unix)]
        let reader_runtime = self.pane_reader_runtime()?;
        if retained_sender.is_some()
            && self
                .pane_outputs
                .get(session_name)
                .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Err(RmuxError::Server(format!(
                "pane output channel already exists for pane id {} in session {}",
                pane_id.as_u32(),
                session_name
            )));
        }
        #[cfg(unix)]
        if let Some(reader) = self
            .pane_output_readers
            .get_mut(session_name)
            .and_then(|panes| panes.remove(&pane_id))
        {
            reader.abort();
        }
        self.transcripts
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, transcript.clone());
        let pane_output = match retained_sender {
            Some(sender) => {
                self.pane_outputs
                    .entry(session_name.clone())
                    .or_default()
                    .insert(pane_id, sender.clone());
                sender
            }
            None => self
                .pane_outputs
                .entry(session_name.clone())
                .or_default()
                .entry(pane_id)
                .or_insert_with(pane_output_channel)
                .clone(),
        };
        let generation = self.advance_pane_output_generation(session_name, pane_id);
        pane_output.set_generation(generation);
        pane_output.clear_retained();
        if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.update_pane_lifecycle_output_sequence(pane_id, generation);
        #[cfg(windows)]
        let eof_state = crate::pane_io::PaneOutputEofState::default();
        #[cfg(windows)]
        if let Some(exit_watcher) = spawn.exit_watcher {
            spawn_pane_exit_watcher(
                session_name.clone(),
                pane_id,
                exit_watcher,
                Some(generation),
                eof_state.clone(),
                spawn.pane_exit_callback.clone(),
            );
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        #[cfg(unix)]
        let output_reader_task = spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
            reader_runtime,
        );
        #[cfg(windows)]
        spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            eof_state,
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
        );
        #[cfg(unix)]
        self.pane_output_readers
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, output_reader_task);
        Ok(())
    }
}

fn seed_initial_pane_title(transcript: &SharedPaneTranscript, initial_title: Option<&str>) {
    let fallback;
    let title = match initial_title.filter(|title| !title.is_empty()) {
        Some(title) => title,
        None => {
            let Some(hostname) = crate::host_name::local_hostname() else {
                return;
            };
            fallback = hostname;
            &fallback
        }
    };
    let mut transcript = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned");
    if transcript.title().is_empty() {
        transcript.set_title(title);
    }
}
