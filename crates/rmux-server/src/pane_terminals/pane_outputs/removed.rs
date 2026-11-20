use std::collections::{HashMap, HashSet};

use rmux_core::PaneId;
use rmux_proto::SessionName;

use crate::pane_io::PaneOutputSender;
use crate::pane_transcript::SharedPaneTranscript;

use super::{AttachedSubmittedLine, PaneExitObservation};
use crate::pane_terminals::HandlerState;

#[derive(Debug, Default)]
pub(in crate::pane_terminals) struct RemovedPaneOutputs {
    pub(super) dead_panes: HashMap<PaneId, PaneExitObservation>,
    pub(super) kept_dead_pane_preparations: HashMap<PaneId, u64>,
    pub(super) transcripts: HashMap<PaneId, SharedPaneTranscript>,
    pub(super) pane_outputs: HashMap<PaneId, PaneOutputSender>,
    retained_pane_outputs: HashSet<PaneId>,
    #[cfg(unix)]
    pane_output_readers: HashMap<PaneId, crate::pane_io::PaneOutputReaderTask>,
    pub(super) pane_output_generations: HashMap<PaneId, u64>,
    pub(super) attached_submitted_rows: HashMap<PaneId, AttachedSubmittedLine>,
}

impl RemovedPaneOutputs {
    pub(in crate::pane_terminals) fn control_colours(
        &self,
        pane_id: PaneId,
    ) -> Option<crate::control_colours::PaneControlColours> {
        self.transcripts
            .get(&pane_id)
            .map(|transcript| transcript.lock().control_colours.clone())
    }

    pub(in crate::pane_terminals) fn pane_output_sender(
        &mut self,
        pane_id: PaneId,
    ) -> Option<PaneOutputSender> {
        if self.pane_outputs.contains_key(&pane_id) {
            self.retained_pane_outputs.insert(pane_id);
        }
        self.pane_outputs.get(&pane_id).cloned()
    }

    pub(in crate::pane_terminals) fn abort_output_readers(&mut self) {
        #[cfg(unix)]
        for (_, reader) in self.pane_output_readers.drain() {
            reader.abort();
        }
        for (pane_id, pane_output) in &self.pane_outputs {
            if !self.retained_pane_outputs.contains(pane_id) {
                pane_output.retire_reader();
            }
        }
    }
}

impl HandlerState {
    pub(in crate::pane_terminals) fn remove_session_pane_outputs(
        &mut self,
        session_name: &SessionName,
    ) -> RemovedPaneOutputs {
        let dead_panes = self.dead_panes.remove(session_name).unwrap_or_default();
        let kept_dead_pane_preparations = self
            .kept_dead_pane_preparations
            .remove(session_name)
            .unwrap_or_default();
        let attached_submitted_rows = self
            .attached_submitted_rows
            .remove(session_name)
            .unwrap_or_default();
        RemovedPaneOutputs {
            dead_panes,
            kept_dead_pane_preparations,
            transcripts: self.transcripts.remove(session_name).unwrap_or_default(),
            pane_outputs: self.pane_outputs.remove(session_name).unwrap_or_default(),
            retained_pane_outputs: HashSet::new(),
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
        if let Some(preparations) = self.kept_dead_pane_preparations.get_mut(session_name) {
            let _ = preparations.remove(&pane_id);
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
        if let Some(pane_output) = &pane_output {
            pane_output.retire_reader();
        }
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
            if let Some(generation) = self
                .kept_dead_pane_preparations
                .get_mut(session_name)
                .and_then(|preparations| preparations.remove(pane_id))
            {
                removed
                    .kept_dead_pane_preparations
                    .insert(*pane_id, generation);
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
        self.kept_dead_pane_preparations
            .entry(session_name.clone())
            .or_default()
            .extend(removed_outputs.kept_dead_pane_preparations);
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
}
