use rmux_core::{PaneGeometry, PaneId, Utf8Config};
use rmux_proto::{RmuxError, SessionName, TerminalSize};
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::PtyMaster;

#[cfg(windows)]
use crate::pane_io::spawn_pane_exit_watcher;
use crate::pane_io::{
    pane_output_channel, spawn_pane_output_reader, PaneAlertCallback, PaneExitCallback,
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
        if let Some(exit_watcher) = spawn.exit_watcher {
            spawn_pane_exit_watcher(
                session_name.clone(),
                pane_id,
                exit_watcher,
                Some(generation),
                spawn.pane_exit_callback.clone(),
            );
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
            #[cfg(unix)]
            reader_runtime,
        );
        Ok(())
    }

    pub(in crate::pane_terminals) fn reset_pane_output(
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
        }
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .mark_clear_on_dead_exit();
        seed_initial_pane_title(&transcript, spawn.initial_title.as_deref());
        #[cfg(unix)]
        let reader_runtime = self.pane_reader_runtime()?;
        self.transcripts
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, transcript.clone());
        let pane_output = self
            .pane_outputs
            .entry(session_name.clone())
            .or_default()
            .entry(pane_id)
            .or_insert_with(pane_output_channel)
            .clone();
        let generation = self.advance_pane_output_generation(session_name, pane_id);
        pane_output.set_generation(generation);
        pane_output.clear_retained();
        if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.update_pane_lifecycle_output_sequence(pane_id, generation);
        #[cfg(windows)]
        if let Some(exit_watcher) = spawn.exit_watcher {
            spawn_pane_exit_watcher(
                session_name.clone(),
                pane_id,
                exit_watcher,
                Some(generation),
                spawn.pane_exit_callback.clone(),
            );
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        spawn_pane_output_reader(
            session_name.clone(),
            pane_id,
            spawn.output_reader,
            transcript,
            pane_output,
            Some(generation),
            spawn.pane_alert_callback,
            spawn.pane_exit_callback,
            #[cfg(unix)]
            reader_runtime,
        );
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
        transcript.append_bytes(format!("\x1b]0;{title}\x07").as_bytes());
    }
}
