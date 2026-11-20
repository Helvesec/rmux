//! The pane lifetime retained by ordinary input while a binding holds it.

use std::io;

use crate::pane_terminals::HandlerState;
use rmux_proto::{PaneTarget, RmuxError, Target};

use super::super::super::super::{require_expected_pane_identity, StableTargetIdentity};
use super::{ActiveAttachIdentity, QueuedUnit, RequestHandler};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler::attach_support) struct QueuedInputIdentity {
    pub(super) attach: ActiveAttachIdentity,
    pane: Option<StableTargetIdentity>,
}

tokio::task_local! {
    static REPLAY_IDENTITY: QueuedInputIdentity;
}

impl RequestHandler {
    pub(super) async fn capture_queued_input_identity(
        &self,
        attach: ActiveAttachIdentity,
    ) -> QueuedInputIdentity {
        if let Ok(identity) = REPLAY_IDENTITY.try_with(Clone::clone) {
            return identity;
        }
        let mut state = self.state.lock().await;
        let active_attach = self.active_attach.lock().await;
        let target = active_attach
            .by_pid
            .get(&attach.attach_pid())
            .filter(|active| attach.matches_active(active))
            .and_then(|active| state.sessions.session(&active.session_name))
            .filter(|session| session.id() == attach.session_id())
            .map(|session| {
                PaneTarget::with_window(
                    session.name().clone(),
                    session.active_window_index(),
                    session.window().active_pane_index(),
                )
            });
        let pane = target.and_then(|target| {
            StableTargetIdentity::capture(&mut state, Target::Pane(target)).ok()
        });
        QueuedInputIdentity { attach, pane }
    }

    pub(super) async fn replay_queued_unit(
        &self,
        identity: QueuedInputIdentity,
        retained: &mut Vec<u8>,
        unit: QueuedUnit,
    ) -> io::Result<bool> {
        if !identity.origin_is_live(&*self.state.lock().await) {
            retained.clear();
            return Ok(false);
        }
        let attach = identity.attach;
        REPLAY_IDENTITY
            .scope(identity, async {
                match unit {
                    QueuedUnit::Live {
                        bytes,
                        windows_console_key,
                    } => {
                        self.replay_queued_attached_input(
                            attach,
                            retained,
                            &bytes,
                            windows_console_key,
                        )
                        .await
                    }
                    QueuedUnit::KeyboardEvent(event) => {
                        // As live: an Escape prefix the replay retained before this event
                        // (`ESC O`) is resolved as at escape-time first, so the event never
                        // overtakes it; a partial UTF-8 character keeps waiting.
                        let mut forwarded = false;
                        if retained.first() == Some(&b'\x1b') {
                            forwarded = self
                                .flush_attached_pending_escape_input_for_identity(attach, retained)
                                .await?;
                            retained.clear();
                        }
                        Ok(forwarded | self.replay_queued_keyboard_event(attach, event).await?)
                    }
                    QueuedUnit::EscapeFlushed(bytes) => {
                        retained.extend_from_slice(&bytes);
                        let result = self
                            .flush_attached_pending_escape_input_for_identity(attach, retained)
                            .await;
                        retained.clear();
                        result
                    }
                }
            })
            .await
    }
}

#[cfg(test)]
impl QueuedInputIdentity {
    pub(super) fn for_test(attach: ActiveAttachIdentity) -> Self {
        Self { attach, pane: None }
    }
}

impl QueuedInputIdentity {
    pub(super) fn continues_origin(&self, previous: &Self, state: &HandlerState) -> bool {
        let Some(previous_pane) = previous
            .pane
            .as_ref()
            .and_then(|pane| pane.resolve_current_pane_identity(state))
        else {
            return false;
        };
        self.attach == previous.attach
            && self
                .pane
                .as_ref()
                .and_then(|pane| pane.resolve_current_pane_identity(state))
                .is_some_and(|pane| pane == previous_pane)
    }

    fn origin_is_live(&self, state: &HandlerState) -> bool {
        self.pane
            .as_ref()
            .and_then(|pane| pane.resolve_current_pane_identity(state))
            .is_some_and(|pane| pane.is_current(state))
    }
}

// Validate at the actual write-preparation lock, while allowing a binding to
// select another live pane. Only a retired origin must invalidate queued input.
pub(in crate::handler) fn check_queued_input_target(
    state: &HandlerState,
    target: &PaneTarget,
) -> Result<(), RmuxError> {
    require_expected_pane_identity(state, target)?;
    if REPLAY_IDENTITY
        .try_with(|identity| identity.origin_is_live(state))
        .unwrap_or(true)
    {
        Ok(())
    } else {
        Err(RmuxError::invalid_target(
            target.to_string(),
            "queued input origin pane was retired",
        ))
    }
}
