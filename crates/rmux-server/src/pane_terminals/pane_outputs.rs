use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use rmux_core::{PaneGeometry, PaneId, Utf8Config};
use rmux_proto::{RmuxError, SessionName};
#[cfg(windows)]
use rmux_pty::PtyChild;
use rmux_pty::PtyMaster;

#[cfg(windows)]
use crate::pane_io::spawn_pane_exit_watcher;
use crate::pane_io::spawn_pane_output_reader;
use crate::pane_io::{pane_output_channel, PaneAlertCallback, PaneExitCallback, PaneOutputSender};
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::pane_transcript::{PaneTranscript, SharedPaneTranscript};

use super::{session_not_found, HandlerState};

#[path = "pane_outputs/submitted.rs"]
mod submitted;

pub(super) use self::submitted::AttachedSubmittedLine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneExitMetadata {
    pub(crate) status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) time: i64,
}

impl PaneExitMetadata {
    fn from_exit_status(status: ExitStatus) -> Self {
        Self {
            status: status.code(),
            signal: exit_signal(status),
            time: chrono::Local::now().timestamp(),
        }
    }
}

#[cfg(unix)]
fn exit_signal(status: ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(windows)]
fn exit_signal(_status: ExitStatus) -> Option<i32> {
    None
}

#[derive(Debug, Default)]
pub(super) struct RemovedPaneOutputs {
    transcripts: HashMap<PaneId, SharedPaneTranscript>,
    pane_outputs: HashMap<PaneId, PaneOutputSender>,
    pane_output_generations: HashMap<PaneId, u64>,
    attached_submitted_rows: HashMap<PaneId, AttachedSubmittedLine>,
}

pub(in crate::pane_terminals) struct PaneOutputSpawn {
    pub(in crate::pane_terminals) geometry: PaneGeometry,
    pub(in crate::pane_terminals) output_reader: PtyMaster,
    #[cfg(windows)]
    pub(in crate::pane_terminals) exit_watcher: Option<PtyChild>,
    pub(in crate::pane_terminals) pane_alert_callback: Option<PaneAlertCallback>,
    pub(in crate::pane_terminals) pane_exit_callback: Option<PaneExitCallback>,
}

impl HandlerState {
    pub(crate) fn observe_runtime_pane_exit(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<PaneExitMetadata>, RmuxError> {
        let Some(target) = self.pane_target_for_runtime_pane(runtime_session_name, pane_id) else {
            return Ok(None);
        };

        if let Some(metadata) = self
            .dead_panes
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .copied()
        {
            self.mark_pane_lifecycle_exited(pane_id, metadata);
            return Ok(Some(metadata));
        }

        let exit_status = self.terminals.pane_exit_status(
            runtime_session_name,
            pane_id,
            target.window_index(),
            target.pane_index(),
        )?;
        let Some(exit_status) = exit_status else {
            return Ok(None);
        };

        let metadata = PaneExitMetadata::from_exit_status(exit_status);
        self.dead_panes
            .entry(runtime_session_name.clone())
            .or_default()
            .insert(pane_id, metadata);
        self.mark_pane_lifecycle_exited(pane_id, metadata);
        Ok(Some(metadata))
    }

    pub(crate) fn append_bytes_to_runtime_pane_transcript(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
        bytes: &[u8],
    ) -> Result<(), RmuxError> {
        let transcript = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .append_bytes(bytes);
        Ok(())
    }

    pub(crate) fn clear_runtime_pane_transcript_for_dead_exit_if_marked(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<bool, RmuxError> {
        let transcript = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        Ok(transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .clear_for_dead_exit_if_marked())
    }

    pub(crate) fn active_pane_output(
        &self,
        session_name: &SessionName,
    ) -> Result<PaneOutputSender, RmuxError> {
        let session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let window_index = session.active_window_index();
        let pane_index = session.active_pane_index();
        let pane_id = session
            .active_pane()
            .map(|pane| pane.id())
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);

        self.pane_outputs
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))
    }

    pub(crate) fn pane_output_for_target(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
    ) -> Result<PaneOutputSender, RmuxError> {
        let pane_id = pane_id_for_target(&self.sessions, session_name, window_index, pane_index)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        self.pane_outputs
            .get(&runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| missing_pane_terminal(session_name, window_index, pane_index))
    }

    pub(crate) fn subscribe_runtime_pane_output(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<tokio::sync::broadcast::Receiver<Vec<u8>>> {
        self.pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .map(PaneOutputSender::subscribe)
    }

    pub(crate) fn session_pane_outputs(
        &self,
        session_name: &SessionName,
    ) -> Result<Vec<(u32, PaneOutputSender)>, RmuxError> {
        let _session = self
            .sessions
            .session(session_name)
            .ok_or_else(|| session_not_found(session_name))?;
        let runtime_session_name = self.runtime_session_name(session_name);
        let Some(pane_outputs) = self.pane_outputs.get(&runtime_session_name) else {
            return Ok(Vec::new());
        };
        let mut outputs = pane_outputs
            .iter()
            .map(|(pane_id, sender)| (pane_id.as_u32(), sender.clone()))
            .collect::<Vec<_>>();
        outputs.sort_by_key(|(pane_id, _)| *pane_id);
        Ok(outputs)
    }

    pub(in crate::pane_terminals) fn insert_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        spawn: PaneOutputSpawn,
    ) -> Result<(), RmuxError> {
        let transcript = PaneTranscript::shared(
            self.history_limit_for_session(session_name),
            rmux_proto::TerminalSize {
                cols: spawn.geometry.cols(),
                rows: spawn.geometry.rows(),
            },
        );
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .set_utf8_config(Utf8Config::from_options(&self.options));
        seed_initial_pane_title(&transcript);
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
            .insert(pane_id, transcript.clone());
        self.pane_outputs
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, pane_output.clone());
        let generation = self.advance_pane_output_generation(session_name, pane_id);
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
            rmux_proto::TerminalSize {
                cols: spawn.geometry.cols(),
                rows: spawn.geometry.rows(),
            },
        );
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .set_utf8_config(Utf8Config::from_options(&self.options));
        transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned")
            .mark_clear_on_dead_exit();
        seed_initial_pane_title(&transcript);
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
        );
        Ok(())
    }

    pub(in crate::pane_terminals) fn remove_session_pane_outputs(
        &mut self,
        session_name: &SessionName,
    ) -> RemovedPaneOutputs {
        let _ = self.dead_panes.remove(session_name);
        let attached_submitted_rows = self
            .attached_submitted_rows
            .remove(session_name)
            .unwrap_or_default();
        RemovedPaneOutputs {
            transcripts: self.transcripts.remove(session_name).unwrap_or_default(),
            pane_outputs: self.pane_outputs.remove(session_name).unwrap_or_default(),
            pane_output_generations: self
                .pane_output_generations
                .remove(session_name)
                .unwrap_or_default(),
            attached_submitted_rows,
        }
    }

    pub(in crate::pane_terminals) fn remove_pane_output(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<(SharedPaneTranscript, PaneOutputSender)> {
        if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
            let _ = dead_panes.remove(&pane_id);
        }
        self.clear_attached_submitted_line(session_name, pane_id);
        if let Some(generations) = self.pane_output_generations.get_mut(session_name) {
            let _ = generations.remove(&pane_id);
        }
        let transcript = self
            .transcripts
            .get_mut(session_name)
            .and_then(|panes| panes.remove(&pane_id));
        let pane_output = self
            .pane_outputs
            .get_mut(session_name)
            .and_then(|panes| panes.remove(&pane_id));

        match (transcript, pane_output) {
            (Some(transcript), Some(pane_output)) => Some((transcript, pane_output)),
            _ => None,
        }
    }

    pub(in crate::pane_terminals) fn remove_pane_outputs(
        &mut self,
        session_name: &SessionName,
        pane_ids: &[PaneId],
    ) -> RemovedPaneOutputs {
        let mut removed = RemovedPaneOutputs::default();
        for pane_id in pane_ids {
            if let Some(dead_panes) = self.dead_panes.get_mut(session_name) {
                let _ = dead_panes.remove(pane_id);
            }
            if let Some(absolute_y) = self.take_attached_submitted_line(session_name, *pane_id) {
                removed.attached_submitted_rows.insert(*pane_id, absolute_y);
            }
            if let Some(transcript) = self
                .transcripts
                .get_mut(session_name)
                .and_then(|panes| panes.remove(pane_id))
            {
                removed.transcripts.insert(*pane_id, transcript);
            }
            if let Some(pane_output) = self
                .pane_outputs
                .get_mut(session_name)
                .and_then(|panes| panes.remove(pane_id))
            {
                removed.pane_outputs.insert(*pane_id, pane_output);
            }
            if let Some(generation) = self
                .pane_output_generations
                .get_mut(session_name)
                .and_then(|panes| panes.remove(pane_id))
            {
                removed.pane_output_generations.insert(*pane_id, generation);
            }
        }
        removed
    }

    pub(in crate::pane_terminals) fn insert_existing_pane_outputs(
        &mut self,
        session_name: &SessionName,
        removed_outputs: RemovedPaneOutputs,
    ) {
        self.attached_submitted_rows
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.attached_submitted_rows);
        self.transcripts
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.transcripts);
        self.pane_outputs
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.pane_outputs);
        self.pane_output_generations
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.pane_output_generations);
    }

    fn advance_pane_output_generation(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> u64 {
        let generations = self
            .pane_output_generations
            .entry(session_name.clone())
            .or_default();
        let next = generations
            .get(&pane_id)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1);
        generations.insert(pane_id, next);
        next
    }

    pub(in crate::pane_terminals) fn pane_output_generation(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
    ) -> u64 {
        self.pane_output_generations
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn move_pane_outputs_between_sessions(
        &mut self,
        source_session: &SessionName,
        destination_session: &SessionName,
        pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        if source_session == destination_session || pane_ids.is_empty() {
            return Ok(());
        }

        let outputs = self.remove_pane_outputs(source_session, pane_ids);
        if outputs.transcripts.len() != pane_ids.len()
            || outputs.pane_outputs.len() != pane_ids.len()
        {
            self.insert_existing_pane_outputs(source_session, outputs);
            return Err(RmuxError::Server(format!(
                "missing pane transcript for transfer from session {source_session}"
            )));
        }

        let destination_transcripts = self
            .transcripts
            .entry(destination_session.clone())
            .or_default();
        let destination_outputs = self
            .pane_outputs
            .entry(destination_session.clone())
            .or_default();
        if pane_ids.iter().any(|pane_id| {
            destination_transcripts.contains_key(pane_id)
                || destination_outputs.contains_key(pane_id)
        }) {
            self.insert_existing_pane_outputs(source_session, outputs);
            return Err(RmuxError::Server(format!(
                "pane transcript already exists in session {destination_session}"
            )));
        }

        self.insert_existing_pane_outputs(destination_session, outputs);
        if let Err(error) =
            self.pipes
                .move_between_sessions(source_session, destination_session, pane_ids)
        {
            let restored = self.remove_pane_outputs(destination_session, pane_ids);
            self.insert_existing_pane_outputs(source_session, restored);
            return Err(error);
        }
        self.refresh_transcript_limits_for_session(destination_session);
        Ok(())
    }

    pub(crate) fn swap_pane_outputs_between_sessions(
        &mut self,
        source_session: &SessionName,
        source_pane_ids: &[PaneId],
        destination_session: &SessionName,
        destination_pane_ids: &[PaneId],
    ) -> Result<(), RmuxError> {
        if source_session == destination_session {
            return Ok(());
        }

        let source_outputs = self.remove_pane_outputs(source_session, source_pane_ids);
        let destination_outputs =
            self.remove_pane_outputs(destination_session, destination_pane_ids);
        if source_outputs.transcripts.len() != source_pane_ids.len()
            || source_outputs.pane_outputs.len() != source_pane_ids.len()
            || destination_outputs.transcripts.len() != destination_pane_ids.len()
            || destination_outputs.pane_outputs.len() != destination_pane_ids.len()
        {
            self.insert_existing_pane_outputs(source_session, source_outputs);
            self.insert_existing_pane_outputs(destination_session, destination_outputs);
            return Err(RmuxError::Server(
                "missing pane transcript for cross-session swap".to_owned(),
            ));
        }

        self.insert_existing_pane_outputs(source_session, destination_outputs);
        self.insert_existing_pane_outputs(destination_session, source_outputs);
        if let Err(error) = self.pipes.swap_between_sessions(
            source_session,
            source_pane_ids,
            destination_session,
            destination_pane_ids,
        ) {
            let restored_source = self.remove_pane_outputs(source_session, destination_pane_ids);
            let restored_destination =
                self.remove_pane_outputs(destination_session, source_pane_ids);
            self.insert_existing_pane_outputs(source_session, restored_destination);
            self.insert_existing_pane_outputs(destination_session, restored_source);
            return Err(error);
        }
        self.refresh_transcript_limits_for_session(source_session);
        self.refresh_transcript_limits_for_session(destination_session);
        Ok(())
    }
}

fn seed_initial_pane_title(transcript: &SharedPaneTranscript) {
    let Some(hostname) = crate::host_name::local_hostname() else {
        return;
    };
    let mut transcript = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned");
    if transcript.title().is_empty() {
        transcript.append_bytes(format!("\x1b]0;{hostname}\x07").as_bytes());
    }
}
