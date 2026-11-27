#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

#[cfg(test)]
use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::HandlerState;
use rmux_core::PaneId;
#[cfg(test)]
use rmux_proto::PaneTarget;
use rmux_proto::{RmuxError, SessionName};

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

    #[cfg(test)]
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

/// Observation follows the output generation through transfers and rollbacks.
/// Public readers only see an exit once its terminal representation commits.
#[derive(Debug, Clone, Copy)]
pub(in crate::pane_terminals) struct PaneExitObservation {
    pub(in crate::pane_terminals) metadata: PaneExitMetadata,
    published: bool,
}

impl PaneExitObservation {
    pub(in crate::pane_terminals) fn observed(metadata: PaneExitMetadata) -> Self {
        Self {
            metadata,
            published: false,
        }
    }

    #[cfg(test)]
    pub(in crate::pane_terminals) fn published(metadata: PaneExitMetadata) -> Self {
        Self {
            metadata,
            published: true,
        }
    }

    pub(in crate::pane_terminals) fn published_metadata(self) -> Option<PaneExitMetadata> {
        self.published.then_some(self.metadata)
    }
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
            .map(|exit| exit.metadata)
        {
            self.expire_runtime_pane_program_status(runtime_session_name, pane_id);
            return Ok(Some(metadata));
        }

        #[cfg(windows)]
        if self
            .starting_panes
            .get(runtime_session_name)
            .is_some_and(|panes| panes.contains_key(&pane_id))
        {
            // A process that already exited ends its pane's start: no console
            // is left to receive the input queued for it.
            if self
                .terminals
                .pane_exit_status(
                    runtime_session_name,
                    pane_id,
                    target.window_index(),
                    target.pane_index(),
                )?
                .is_none()
            {
                return Ok(None);
            }
            let _ = self.cancel_starting_pane(runtime_session_name, pane_id);
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
            .insert(pane_id, PaneExitObservation::observed(metadata));
        self.expire_runtime_pane_program_status(runtime_session_name, pane_id);
        Ok(Some(metadata))
    }

    fn expire_runtime_pane_program_status(
        &self,
        runtime_session_name: &SessionName,
        pane_id: PaneId,
    ) {
        if let Some(transcript) = self
            .transcripts
            .get(runtime_session_name)
            .and_then(|panes| panes.get(&pane_id))
        {
            transcript.lock_for_exit().expire_program_status();
        }
    }

    #[cfg(test)]
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
            .entry(runtime_session_name.clone())
            .or_default()
            .insert(pane_id, PaneExitObservation::published(metadata));
        self.mark_pane_lifecycle_exited(pane_id, metadata);
        self.expire_runtime_pane_program_status(&runtime_session_name, pane_id);
        Ok(())
    }

    pub(crate) fn observed_runtime_pane_exit_metadata(
        &self,
        session: &SessionName,
        pane_id: PaneId,
    ) -> Option<PaneExitMetadata> {
        self.dead_panes
            .get(session)?
            .get(&pane_id)
            .map(|exit| exit.metadata)
    }

    pub(crate) fn publish_runtime_pane_exit(&mut self, session: &SessionName, pane_id: PaneId) {
        let Some(exit) = self
            .dead_panes
            .get_mut(session)
            .and_then(|panes| panes.get_mut(&pane_id))
        else {
            return;
        };
        exit.published = true;
        let metadata = exit.metadata;
        self.mark_pane_lifecycle_exited(pane_id, metadata);
    }

    /// Capture a removed pane's exit formats without publishing an exit while
    /// teardown can still fail and leave a visible pane without its footer.
    pub(crate) fn with_pane_exit_context<T>(
        &mut self,
        session: &SessionName,
        pane_id: PaneId,
        capture: impl FnOnce(&Self) -> T,
    ) -> T {
        let observation = self
            .dead_panes
            .get(session)
            .and_then(|panes| panes.get(&pane_id))
            .copied();
        let lifecycle = self.pane_lifecycle.get(&pane_id).cloned();
        self.publish_runtime_pane_exit(session, pane_id);
        let result = capture(self);
        if let Some(observation) = observation {
            self.dead_panes
                .get_mut(session)
                .expect("exit session exists")
                .insert(pane_id, observation);
        }
        if let Some(lifecycle) = lifecycle {
            self.pane_lifecycle.insert(pane_id, lifecycle);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use rmux_proto::{SessionName, TerminalSize};

    use super::{PaneExitMetadata, PaneExitObservation};
    use crate::pane_terminals::HandlerState;
    use crate::pane_transcript::PaneTranscript;

    #[test]
    fn observing_an_already_recorded_exit_expires_active_program_status() {
        let session_name = SessionName::new("alpha").expect("valid session");
        let size = TerminalSize { cols: 80, rows: 24 };
        let mut state = HandlerState::default();
        state
            .sessions
            .create_session(session_name.clone(), size)
            .expect("session should be created");
        let pane_id = state
            .sessions
            .session(&session_name)
            .and_then(|session| session.window_at(0))
            .and_then(|window| window.pane(0))
            .map(|pane| pane.id())
            .expect("initial pane exists");
        let transcript = PaneTranscript::shared(2_000, size);
        transcript
            .lock()
            .append_bytes(b"\x1b]7501;state=working:app=pi\x07\x1b]7501;state=done:id=result\x07");
        assert_eq!(transcript.lock().program_status().records().len(), 2);
        state
            .transcripts
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, transcript.clone());
        let metadata = PaneExitMetadata::without_exit_details();
        state
            .dead_panes
            .entry(session_name.clone())
            .or_default()
            .insert(pane_id, PaneExitObservation::observed(metadata));

        let observed = state
            .observe_runtime_pane_exit(&session_name, pane_id)
            .expect("recorded exit is observable");

        assert_eq!(observed, Some(metadata));
        assert_eq!(
            transcript.lock().program_status().records().len(),
            1,
            "observing a recorded pane exit must expire its active program status"
        );
    }
}
