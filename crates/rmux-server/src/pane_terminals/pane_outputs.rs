use std::collections::HashSet;

use rmux_core::{events::PaneOutputSubscriptionKey, PaneId};
use rmux_proto::{PaneTarget, RmuxError, SessionName};

use crate::pane_io::PaneOutputSender;
use crate::pane_terminal_lookup::{missing_pane_terminal, pane_id_for_target};

use super::{session_not_found, HandlerState};

mod submitted;

mod exit_refresh;
mod removed;
mod spawn;

pub(super) use self::removed::RemovedPaneOutputs;
pub(in crate::pane_terminals) use self::spawn::PaneOutputSpawn;
pub(super) use self::submitted::AttachedSubmittedLine;

mod exit_status;
pub(crate) use exit_status::PaneExitMetadata;
pub(in crate::pane_terminals) use exit_status::PaneExitObservation;

impl HandlerState {
    pub(crate) fn has_pane_output(&self, pane_id: u32) -> bool {
        self.pane_outputs
            .values()
            .any(|panes| panes.keys().any(|candidate| candidate.as_u32() == pane_id))
    }

    pub(crate) fn pane_output_ids(&self) -> HashSet<u32> {
        self.pane_outputs
            .values()
            .flat_map(|panes| panes.keys())
            .map(|pane_id| pane_id.as_u32())
            .collect()
    }

    pub(crate) fn append_bytes_to_runtime_pane_transcript(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
        bytes: &[u8],
    ) -> Result<(), RmuxError> {
        self.mutate_runtime_pane_transcript(runtime_session_name, pane_id, !bytes.is_empty(), |t| {
            t.append_bytes(bytes);
        })
    }

    pub(crate) fn mutate_runtime_pane_transcript(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
        changed: bool,
        mutation: impl FnOnce(&mut crate::pane_transcript::PaneTranscript),
    ) -> Result<(), RmuxError> {
        let transcript = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        if let Some(output) = self
            .pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
        {
            output.mutate_transcript(
                &transcript,
                crate::pane_io::PaneInvalidationReason::TranscriptMutation,
                |transcript| (mutation(transcript), changed),
            );
        } else {
            mutation(&mut transcript.lock());
        }
        Ok(())
    }

    pub(crate) fn hide_runtime_pane_cursor(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let transcript = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        if let Some(output) = self
            .pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
        {
            output.mutate_transcript(
                &transcript,
                crate::pane_io::PaneInvalidationReason::TranscriptMutation,
                |transcript| ((), transcript.hide_cursor()),
            );
        } else {
            transcript.lock().hide_cursor();
        }
        Ok(())
    }

    /// Publishes synthetic pane bytes and applies them to the transcript at
    /// the same output-state linearization point as PTY output.
    #[cfg(windows)]
    pub(crate) fn publish_bytes_to_runtime_pane_transcript(
        &mut self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        bytes: Vec<u8>,
    ) -> Result<bool, RmuxError> {
        let transcript = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane transcript for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        let output = self
            .pane_outputs
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
            .cloned()
            .ok_or_else(|| {
                RmuxError::Server(format!(
                    "missing pane output for pane id {} in session {}",
                    pane_id.as_u32(),
                    runtime_session_name
                ))
            })?;
        Ok(output
            .publish_for_generation_with_invalidation(generation, bytes, |bytes| {
                let append = transcript.lock().append_bytes_with_effects(bytes);
                let invalidation = append
                    .recovery_rebase_required
                    .then_some(crate::pane_io::PaneInvalidationReason::TranscriptMutation);
                ((), Vec::new(), invalidation)
            })
            .is_some())
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

    pub(crate) fn kept_dead_pane_prepared_for_generation(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        generation: u64,
    ) -> bool {
        self.kept_dead_pane_preparations
            .get(session_name)
            .and_then(|panes| panes.get(&pane_id))
            .is_some_and(|prepared_generation| *prepared_generation == generation)
    }

    pub(crate) fn mark_kept_dead_pane_prepared(
        &mut self,
        session_name: &SessionName,
        pane_id: PaneId,
        generation: u64,
    ) {
        self.kept_dead_pane_preparations
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
        Ok(())
    }
}
