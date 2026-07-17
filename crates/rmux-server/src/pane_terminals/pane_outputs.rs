use std::collections::HashMap;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use rmux_core::{events::PaneOutputSubscriptionKey, PaneId};
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use crate::pane_io::PaneOutputSender;
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};
use crate::pane_transcript::SharedPaneTranscript;

use super::{session_not_found, HandlerState};

#[path = "pane_outputs/submitted.rs"]
mod submitted;

#[path = "pane_outputs/exit_refresh.rs"]
mod exit_refresh;
#[path = "pane_outputs/spawn.rs"]
mod spawn;

pub(in crate::pane_terminals) use self::spawn::PaneOutputSpawn;
pub(super) use self::submitted::AttachedSubmittedLine;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneExitMetadata {
    pub(crate) status: Option<i32>,
    pub(crate) signal: Option<i32>,
    pub(crate) time: Option<i64>,
}

impl PaneExitMetadata {
    fn from_exit_status(status: ExitStatus) -> Self {
        Self {
            status: status.code(),
            signal: exit_signal(status),
            time: Some(chrono::Local::now().timestamp()),
        }
    }

    pub(crate) const fn without_exit_details() -> Self {
        Self {
            status: None,
            signal: None,
            time: None,
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
    dead_panes: HashMap<PaneId, PaneExitMetadata>,
    transcripts: HashMap<PaneId, SharedPaneTranscript>,
    pane_outputs: HashMap<PaneId, PaneOutputSender>,
    #[cfg(unix)]
    pane_output_readers: HashMap<PaneId, crate::pane_io::PaneOutputReaderTask>,
    pane_output_generations: HashMap<PaneId, u64>,
    attached_submitted_rows: HashMap<PaneId, AttachedSubmittedLine>,
}

impl RemovedPaneOutputs {
    pub(in crate::pane_terminals) fn pane_output_sender(
        &self,
        pane_id: PaneId,
    ) -> Option<PaneOutputSender> {
        self.pane_outputs.get(&pane_id).cloned()
    }

    pub(in crate::pane_terminals) fn abort_output_readers(&mut self) {
        #[cfg(unix)]
        for (_, reader) in self.pane_output_readers.drain() {
            reader.abort();
        }
    }
}

impl HandlerState {
    pub(crate) fn observe_runtime_pane_exit(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<Option<PaneExitMetadata>, RmuxError> {
        #[cfg(windows)]
        if self
            .starting_panes
            .get(runtime_session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            return Ok(None);
        }

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

    pub(crate) fn mark_pane_dead_without_exit_details(
        &mut self,
        target: &PaneTarget,
    ) -> Result<(), RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        let metadata = PaneExitMetadata::without_exit_details();
        self.dead_panes
            .entry(runtime_session_name)
            .or_default()
            .insert(pane_id, metadata);
        self.mark_pane_lifecycle_exited(pane_id, metadata);
        Ok(())
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

    pub(crate) fn pane_output_subscription_key_for_target(
        &self,
        target: &rmux_proto::PaneTarget,
    ) -> Result<PaneOutputSubscriptionKey, RmuxError> {
        let pane_id = pane_id_for_target(
            &self.sessions,
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )?;
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        Ok(PaneOutputSubscriptionKey::new(
            runtime_session_name,
            pane_id,
        ))
    }

    pub(crate) fn pane_output_subscription_key_for_pane_id(
        &self,
        pane_id: PaneId,
    ) -> Option<PaneOutputSubscriptionKey> {
        self.pane_alias_targets(pane_id)
            .into_iter()
            .find_map(|target| self.pane_output_subscription_key_for_target(&target).ok())
    }

    pub(crate) fn pane_output_subscription_keys_for_kill(
        &self,
        target: &rmux_proto::PaneTarget,
        kill_all_except: bool,
    ) -> Result<Vec<PaneOutputSubscriptionKey>, RmuxError> {
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
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        let keys = window
            .panes()
            .iter()
            .filter(|pane| {
                if kill_all_except {
                    pane.index() != target.pane_index()
                } else {
                    pane.index() == target.pane_index()
                }
            })
            .map(|pane| PaneOutputSubscriptionKey::new(runtime_session_name.clone(), pane.id()))
            .collect();
        Ok(keys)
    }

    pub(crate) fn subscribe_runtime_pane_output(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::pane_io::PaneOutputReceiver> {
        self.pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .map(PaneOutputSender::subscribe)
    }

    #[cfg(windows)]
    pub(crate) fn subscribe_runtime_pane_output_from_oldest(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Option<crate::pane_io::PaneOutputReceiver> {
        self.pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .map(PaneOutputSender::subscribe_from_oldest)
    }

    pub(crate) fn runtime_pane_output_drain_handles(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> (
        Option<crate::pane_io::PaneOutputReceiver>,
        Option<PaneOutputSender>,
    ) {
        let sender = self
            .pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned();
        let receiver = sender.as_ref().map(PaneOutputSender::subscribe);
        (receiver, sender)
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

    pub(in crate::pane_terminals) fn remove_session_pane_outputs(
        &mut self,
        session_name: &SessionName,
    ) -> RemovedPaneOutputs {
        let dead_panes = self.dead_panes.remove(session_name).unwrap_or_default();
        let attached_submitted_rows = self
            .attached_submitted_rows
            .remove(session_name)
            .unwrap_or_default();
        RemovedPaneOutputs {
            dead_panes,
            transcripts: self.transcripts.remove(session_name).unwrap_or_default(),
            pane_outputs: self.pane_outputs.remove(session_name).unwrap_or_default(),
            #[cfg(unix)]
            pane_output_readers: self
                .pane_output_readers
                .remove(session_name)
                .unwrap_or_default(),
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
        #[cfg(unix)]
        if let Some(reader) = self
            .pane_output_readers
            .get_mut(session_name)
            .and_then(|panes| panes.remove(&pane_id))
        {
            reader.abort();
        }

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
            if let Some(metadata) = self
                .dead_panes
                .get_mut(session_name)
                .and_then(|dead_panes| dead_panes.remove(pane_id))
            {
                removed.dead_panes.insert(*pane_id, metadata);
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
            #[cfg(unix)]
            if let Some(reader) = self
                .pane_output_readers
                .get_mut(session_name)
                .and_then(|panes| panes.remove(pane_id))
            {
                removed.pane_output_readers.insert(*pane_id, reader);
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
        self.dead_panes
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.dead_panes);
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
        #[cfg(unix)]
        self.pane_output_readers
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.pane_output_readers);
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
            .saturating_add(1);
        generations.insert(pane_id, next);
        next
    }

    pub(crate) fn pane_output_generation(
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

    pub(crate) fn pane_output_generation_for_target(
        &self,
        target: &PaneTarget,
        pane_id: PaneId,
    ) -> u64 {
        let runtime_session_name =
            self.runtime_session_name_for_window(target.session_name(), target.window_index());
        self.pane_output_generation(&runtime_session_name, pane_id)
    }

    pub(in crate::pane_terminals) fn seed_pane_output_generation(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        generation: u64,
    ) {
        self.pane_output_generations
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, generation);
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
