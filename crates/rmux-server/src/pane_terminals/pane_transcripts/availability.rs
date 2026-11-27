//! Pane availability checked before a layout change commits.

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, RmuxError, SessionName, WindowTarget};

use crate::pane_terminal_lookup::pane_id_for_target;
use crate::pane_terminals::HandlerState;

impl HandlerState {
    /// Refuses a request that names `pane_id`, a pane of `session_name`'s
    /// window `window_index`, while its transcript is poisoned. The resize that
    /// follows a layout change reads the window's other panes under the sweep
    /// policy, so the pane a request names fails fast here, before the change
    /// it made stays committed.
    pub(crate) fn ensure_pane_id_available(
        &self,
        session_name: &SessionName,
        window_index: u32,
        pane_id: PaneId,
    ) -> Result<(), RmuxError> {
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(transcript) = self
            .transcripts
            .get(&runtime_session_name)
            .and_then(|transcripts| transcripts.get(&pane_id))
        else {
            return Ok(());
        };
        transcript
            .ensure_available()
            .map_err(|unavailable| RmuxError::Message(unavailable.to_string()))
    }

    /// [`Self::ensure_pane_id_available`] for the pane `pane` names; a missing
    /// pane is left to the request's own lookup.
    pub(crate) fn ensure_pane_available(&self, pane: &PaneTarget) -> Result<(), RmuxError> {
        let Ok(pane_id) = pane_id_for_target(
            &self.sessions,
            pane.session_name(),
            pane.window_index(),
            pane.pane_index(),
        ) else {
            return Ok(());
        };
        self.ensure_pane_id_available(pane.session_name(), pane.window_index(), pane_id)
    }

    /// The one availability contract of `join-pane`, `move-pane`, `swap-pane`
    /// and `break-pane` for the panes they name. tmux 3.7b answers a transfer
    /// that moves nothing whatever its panes hold: a pane joined, moved or
    /// swapped with itself, unless the swap unzooms its window, and a break
    /// onto a window index that is taken. A poisoned named pane is answered
    /// there as a healthy one. Every other transfer resizes the windows of the
    /// panes it names, so one naming a poisoned pane fails fast before
    /// anything changes.
    pub(crate) fn ensure_transfer_panes_available(
        &self,
        transfer: NamedPaneTransfer<'_>,
    ) -> Result<(), RmuxError> {
        let (source, target) = match transfer {
            NamedPaneTransfer::JoinOrMove { source, target } => {
                if self.names_one_pane(source, target) {
                    return Ok(());
                }
                (source, Some(target))
            }
            NamedPaneTransfer::Swap {
                source,
                target,
                preserve_zoom,
            } => {
                if self.names_one_pane(source, target)
                    && (preserve_zoom || !self.window_is_zoomed(source))
                {
                    return Ok(());
                }
                (source, Some(target))
            }
            NamedPaneTransfer::Break {
                source,
                destination,
            } => {
                if destination.is_some_and(|window| self.window_index_is_taken(window)) {
                    return Ok(());
                }
                (source, None)
            }
        };
        self.ensure_pane_available(source)?;
        target.map_or(Ok(()), |target| self.ensure_pane_available(target))
    }

    fn names_one_pane(&self, first: &PaneTarget, second: &PaneTarget) -> bool {
        let pane_id = |pane: &PaneTarget| {
            pane_id_for_target(
                &self.sessions,
                pane.session_name(),
                pane.window_index(),
                pane.pane_index(),
            )
            .ok()
        };
        pane_id(first).is_some_and(|first| pane_id(second) == Some(first))
    }

    fn window_is_zoomed(&self, pane: &PaneTarget) -> bool {
        self.sessions
            .session(pane.session_name())
            .and_then(|session| session.window_at(pane.window_index()))
            .is_some_and(|window| window.is_zoomed())
    }

    fn window_index_is_taken(&self, window: &WindowTarget) -> bool {
        self.sessions
            .session(window.session_name())
            .is_some_and(|session| session.window_at(window.window_index()).is_some())
    }
}

/// The panes a pane transfer names, as its command resolved them.
pub(crate) enum NamedPaneTransfer<'a> {
    /// `join-pane` or `move-pane`: the source joins the target's window.
    JoinOrMove {
        source: &'a PaneTarget,
        target: &'a PaneTarget,
    },
    /// `swap-pane`; `preserve_zoom` is its `-Z`.
    Swap {
        source: &'a PaneTarget,
        target: &'a PaneTarget,
        preserve_zoom: bool,
    },
    /// `break-pane`; `destination` is the window index it creates, absent
    /// when it inserts beside its target with `-a` or `-b`.
    Break {
        source: &'a PaneTarget,
        destination: Option<&'a WindowTarget>,
    },
}
