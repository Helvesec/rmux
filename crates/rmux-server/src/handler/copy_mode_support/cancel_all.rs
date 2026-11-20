use super::RequestHandler;
use crate::handler::mode_tree_support::ModeTreePaneIdentity;
use crate::pane_terminals::HandlerState;
use crate::pane_transcript::{PaneModeKind, PaneTranscript, SharedPaneTranscript};
use rmux_proto::{PaneTarget, RmuxError, SessionId};

/// What one `-q` keeps of the pane it started on.
///
/// The identity is captured ONCE, under the locks, and is what every later pop
/// is checked against: session, window, occurrence, pane id and output
/// generation, plus the transcript instance those modes are really installed on.
/// `expected_revision` is the revision a pop is allowed to act on - the one the
/// previous pop committed - so a mode that entered in between, or a pane that
/// took the same numeric slot, is never mistaken for what this command was
/// walking.
pub(in crate::handler) struct PaneModeTransition {
    identity: ModeTreePaneIdentity,
    transcript: SharedPaneTranscript,
    expected_revision: u64,
    kind: PaneModeKind,
}

impl PaneModeTransition {
    pub(in crate::handler) fn target(&self) -> &PaneTarget {
        self.identity.target()
    }

    pub(in crate::handler) const fn transcript(&self) -> &SharedPaneTranscript {
        &self.transcript
    }

    /// Whether this is still the pane this `-q` started on, on the same
    /// transcript instance. Those live in `state` and are answered there.
    pub(in crate::handler) fn matches_identity(&self, state: &HandlerState) -> bool {
        self.identity.matches(state)
            && state
                .transcript_handle(self.identity.target())
                .is_ok_and(|current| current.same_instance(&self.transcript))
    }

    /// The revision, the kind, the pop itself and what it left - in ONE guard.
    ///
    /// Nothing another writer does can fall between them. A mode that entered
    /// while this command was awaiting its own effects therefore neither passes
    /// for the one being walked nor is adopted as what this pop left behind: the
    /// revision no longer matches and nothing is popped at all.
    pub(in crate::handler) fn pop(
        &self,
        transcript: &mut PaneTranscript,
    ) -> Option<PaneModePopReceipt> {
        if transcript.pane_mode_revision() != self.expected_revision
            || transcript.pane_mode_kind() != Some(self.kind)
        {
            return None;
        }
        let popped_name = transcript.pane_mode_name()?;
        let popped = match self.kind {
            PaneModeKind::Copy => transcript.clear_copy_mode(),
            PaneModeKind::Clock => transcript.clear_clock_mode(),
            PaneModeKind::ModeTree => transcript.clear_mode_tree(),
        };
        if !popped {
            return None;
        }
        Some(PaneModePopReceipt {
            revision: transcript.pane_mode_revision(),
            lower: transcript.pane_mode_kind(),
            popped_name,
        })
    }
}

/// What a pop committed: the revision it left, and the one mode underneath it
/// that this `-q` may go on to leave.
pub(in crate::handler) struct PaneModePopReceipt {
    revision: u64,
    lower: Option<PaneModeKind>,
    popped_name: &'static str,
}

impl PaneModePopReceipt {
    pub(in crate::handler) const fn revision(&self) -> u64 {
        self.revision
    }

    /// What was popped, named as the pane names it, read in the same guard.
    /// A rollback puts back exactly that.
    pub(in crate::handler) const fn popped_name(&self) -> &'static str {
        self.popped_name
    }

    /// Whether the mode this pop uncovered is a consultation.
    ///
    /// It is read from the receipt the pop itself produced, under the guard that
    /// popped: what the pane reserves for a modal scrollbar depends on this and
    /// on nothing else, so a caller that needs to know whether the reserve can
    /// have changed asks the pop, never the transcript again afterwards.
    pub(in crate::handler) const fn uncovered_copy_mode(&self) -> bool {
        matches!(self.lower, Some(PaneModeKind::Copy))
    }

    /// The historic clock exit, which answers for whatever it finds rather than
    /// for a revision a walk is carrying. Same single guard.
    pub(in crate::handler) fn pop_clock(transcript: &mut PaneTranscript) -> Option<Self> {
        let popped_name = transcript.pane_mode_name()?;
        if !transcript.clear_clock_mode() {
            return None;
        }
        Some(Self {
            revision: transcript.pane_mode_revision(),
            lower: transcript.pane_mode_kind(),
            popped_name,
        })
    }
}

impl RequestHandler {
    /// Leaves EVERY mode of the targeted pane, the way `copy-mode -q` does.
    ///
    /// Each mode leaves through the path that already owns its effects: the
    /// consultation through its own clearing and refresh, the clock through its
    /// exit - identities, restored frame, notifications, and a timer left stale
    /// by the generation that disappears - and a tree through the dismissal that
    /// restores the zoom, the client state and the mode it was opened over. None
    /// of those paths changes for the callers that keep using them one at a time,
    /// and `send-keys -X cancel` keeps its unitary pop
    /// (`window-copy.c:3519-3520`).
    ///
    /// Between two pops those effects are awaited, and the pane can change hands
    /// meanwhile. What may be popped next is therefore only what the pop just
    /// committed left underneath, on the identity and revision that commit
    /// recorded: a consultation that re-entered afterwards, or a pane that took
    /// the slot, belongs to another transition and this one ends instead.
    pub(super) async fn leave_every_pane_mode(
        &self,
        target: &PaneTarget,
        session_id: SessionId,
    ) -> Result<(), RmuxError> {
        let Some(mut transition) = self.capture_pane_mode_transition(target).await? else {
            return Ok(());
        };
        loop {
            let popped = match transition.kind {
                PaneModeKind::ModeTree => {
                    self.dismiss_mode_trees_hosted_by_pane(&transition).await?
                }
                PaneModeKind::Clock => self.exit_clock_mode_for_transition(&transition).await?,
                PaneModeKind::Copy => self.leave_copy_mode(&transition, session_id).await?,
            };
            // Nothing was committed: this pane, its transcript or its revision is
            // no longer the one being walked, so the walk stops without mutating.
            let Some(receipt) = popped else {
                return Ok(());
            };
            let Some(lower) = receipt.lower else {
                return Ok(());
            };
            transition.expected_revision = receipt.revision;
            transition.kind = lower;
        }
    }

    /// The pane this `-q` addresses, captured once, under the state lock.
    async fn capture_pane_mode_transition(
        &self,
        target: &PaneTarget,
    ) -> Result<Option<PaneModeTransition>, RmuxError> {
        let mut state = self.state.lock().await;
        let transcript = state.transcript_handle(target)?;
        let identity = ModeTreePaneIdentity::capture(&mut state, target)?;
        let (kind, expected_revision) = {
            let transcript = transcript.lock();
            (transcript.pane_mode_kind(), transcript.pane_mode_revision())
        };
        Ok(kind.map(|kind| PaneModeTransition {
            identity,
            transcript,
            expected_revision,
            kind,
        }))
    }

    /// The consultation's own leaving, with the resize and the refresh fanout
    /// the cancel path has always done.
    async fn leave_copy_mode(
        &self,
        transition: &PaneModeTransition,
        session_id: SessionId,
    ) -> Result<Option<PaneModePopReceipt>, RmuxError> {
        let target = transition.target().clone();
        let receipt = {
            let state = self.state.lock().await;
            if !transition.matches_identity(&state) {
                return Ok(None);
            }
            let Some(receipt) = transition.pop(&mut transition.transcript().lock()) else {
                return Ok(None);
            };
            receipt
        };
        self.state
            .lock()
            .await
            .resize_terminals(target.session_name())?;
        let identities = self
            .prepare_copy_mode_refresh_fanout(&target, session_id, true)
            .await?;
        self.refresh_copy_mode_session_identities(identities).await;
        Ok(Some(receipt))
    }
}
