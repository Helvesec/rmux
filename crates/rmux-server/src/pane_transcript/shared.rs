use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use rmux_core::{PaneId, Screen};
use rmux_proto::TerminalSize;

use super::PaneTranscript;

/// Shared transcript identity and its operation-specific poison policy.
/// Guards preserve callers' critical sections; the mutex itself stays private.
#[derive(Clone, Debug)]
pub(crate) struct SharedPaneTranscript {
    inner: Arc<Mutex<PaneTranscript>>,
    pane_id: PaneId,
}

/// Panic payload of [`SharedPaneTranscript::lock`] on a poisoned transcript.
///
/// The operation still fails fast; the payload only names the pane so a
/// request boundary can answer its client instead of dropping the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneTranscriptUnavailable {
    pane_id: PaneId,
}

impl PaneTranscriptUnavailable {
    /// The same answer for a pane whose output state, rather than its
    /// transcript, is poisoned.
    pub(crate) const fn for_pane(pane_id: PaneId) -> Self {
        Self { pane_id }
    }

    pub(crate) const fn pane_id(self) -> PaneId {
        self.pane_id
    }
}

impl std::fmt::Display for PaneTranscriptUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "pane {} unavailable", self.pane_id)
    }
}

/// How a format reaches the pane whose transcript it reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneTranscriptReach {
    /// The request named this pane: a poisoned transcript fails fast through
    /// [`SharedPaneTranscript::lock`], so its client reads the typed
    /// [`PaneTranscriptUnavailable`] error instead of an empty expansion.
    /// Beyond a request boundary it observes, as [`Self::Reached`] does.
    Named,
    /// A listing, loop, tree or status line reached this pane on its way to
    /// others: a poisoned transcript observes as unavailable and expands empty.
    Reached,
}

tokio::task_local! {
    /// Set while a request boundary polls its dispatch and answers a
    /// [`PaneTranscriptUnavailable`] abort; cleared again for the hooks the
    /// request runs, which must never fail it.
    static UNAVAILABLE_PANE_ANSWERED: bool;
}

/// Runs `future` under a boundary that answers a [`PaneTranscriptUnavailable`]
/// abort, so the pane a format names may fail fast through
/// [`SharedPaneTranscript::lock`].
pub(crate) async fn with_unavailable_pane_answered<F: Future>(future: F) -> F::Output {
    UNAVAILABLE_PANE_ANSWERED.scope(true, future).await
}

/// Runs `future` beyond any request boundary, as a hook does: a format that
/// names a poisoned pane observes it and expands empty.
pub(crate) async fn with_unavailable_pane_unanswered<F: Future>(future: F) -> F::Output {
    UNAVAILABLE_PANE_ANSWERED.scope(false, future).await
}

fn unavailable_pane_is_answered() -> bool {
    UNAVAILABLE_PANE_ANSWERED
        .try_with(|answered| *answered)
        .unwrap_or(false)
}

impl SharedPaneTranscript {
    pub(super) fn new(pane_id: PaneId, transcript: PaneTranscript) -> Self {
        Self {
            inner: Arc::new(Mutex::new(transcript)),
            pane_id,
        }
    }

    /// Ordinary reads and writes fail fast on a poisoned transcript. The panic
    /// carries [`PaneTranscriptUnavailable`] rather than a message.
    pub(crate) fn lock(&self) -> MutexGuard<'_, PaneTranscript> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(_) => std::panic::panic_any(PaneTranscriptUnavailable {
                pane_id: self.pane_id,
            }),
        }
    }

    /// The typed error [`Self::lock`] would abort with, checked without
    /// locking. A request checks it before it commits a change whose aftermath
    /// reads this transcript through [`Self::lock`].
    pub(crate) fn ensure_available(&self) -> Result<(), PaneTranscriptUnavailable> {
        if self.inner.is_poisoned() {
            return Err(PaneTranscriptUnavailable {
                pane_id: self.pane_id,
            });
        }
        Ok(())
    }

    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Mutations under PaneOutputState recover so parser timers can progress
    /// and the mutation and its invalidation keep one publication boundary.
    pub(crate) fn lock_for_output_mutation(&self) -> MutexGuard<'_, PaneTranscript> {
        self.recover()
    }

    /// EOF must release synchronized output before closing. The caller still
    /// owns the generation check and output publication boundary.
    pub(crate) fn lock_for_eof(&self) -> MutexGuard<'_, PaneTranscript> {
        self.recover()
    }

    /// Exit observation expires program status on the way to teardown. Poison
    /// must not abort the exit and leave a finished pane listed.
    pub(crate) fn lock_for_exit(&self) -> MutexGuard<'_, PaneTranscript> {
        self.recover()
    }

    /// Raw/surface streams and Web capture bounded recovery state under the
    /// output observer boundary even when ordinary transcript reads fail.
    pub(crate) fn lock_for_recovery_capture(&self) -> MutexGuard<'_, PaneTranscript> {
        self.recover()
    }

    /// Repeated geometry can be rejected despite poison, before taking output
    /// state. The caller rechecks at mutation under that path's own policy.
    pub(crate) fn needs_resize(&self, size: TerminalSize) -> bool {
        self.recover().screen().size() != size
    }

    /// Cross-pane sweeps, option propagation and format observation run for
    /// requests that never named this pane. They skip a poisoned transcript
    /// instead of failing every request, without serving its state, while the
    /// pane's own operations keep failing fast through [`Self::lock`].
    pub(crate) fn lock_for_cross_pane_sweep(&self) -> Option<MutexGuard<'_, PaneTranscript>> {
        self.inner.lock().ok()
    }

    /// Format observation under the policy of how the request reached the pane.
    /// A named pane fails fast only where a request boundary answers the
    /// abort; hooks, timers and background tasks observe it instead.
    pub(crate) fn lock_for_format(
        &self,
        reach: PaneTranscriptReach,
    ) -> Option<MutexGuard<'_, PaneTranscript>> {
        match reach {
            PaneTranscriptReach::Named if unavailable_pane_is_answered() => Some(self.lock()),
            PaneTranscriptReach::Named | PaneTranscriptReach::Reached => {
                self.lock_for_cross_pane_sweep()
            }
        }
    }

    /// Preview discards a poisoned initial read; its caller owns the fallback.
    pub(crate) fn preview_screen(&self) -> Option<Screen> {
        self.inner
            .lock()
            .ok()
            .map(|transcript| transcript.clone_presentation_screen())
    }

    fn recover(&self) -> MutexGuard<'_, PaneTranscript> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[cfg(test)]
    pub(crate) fn lock_unavailable_for_test(&self) -> bool {
        self.inner.try_lock().is_err()
    }

    #[cfg(test)]
    pub(crate) fn poison_for_test(&self) {
        assert!(std::panic::catch_unwind(|| {
            let _guard = self.lock();
            panic!("poison transcript for policy test");
        })
        .is_err());
    }
}

#[cfg(test)]
mod tests;
