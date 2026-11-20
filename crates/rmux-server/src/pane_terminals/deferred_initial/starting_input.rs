//! Bounded input queue of panes whose deferred initial spawn is still starting.

use rmux_core::PaneId;
use rmux_proto::{RmuxError, SessionName};

use super::{
    DeferredInitialPaneConsoleInputAction, DeferredInitialPaneInput, HandlerState, PasteDelimiters,
    STARTING_PANE_INPUT_MAX_BYTES,
};

/// Why queued startup input is discarded when its pane is no longer starting.
pub(crate) const DEFERRED_INPUT_PANE_STOPPED: &str =
    "deferred pane input was not delivered: the pane stopped starting";

/// How much queued startup input a pane never received.
///
/// Only counts are kept: queued input may be a typed secret, so its content
/// never reaches the message log.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DeferredInputTally {
    inputs: usize,
    bytes: usize,
}

impl DeferredInputTally {
    pub(crate) fn of<'a>(queued: impl IntoIterator<Item = &'a DeferredInitialPaneInput>) -> Self {
        queued
            .into_iter()
            .fold(Self::default(), |tally, input| Self {
                inputs: tally.inputs + 1,
                bytes: tally.bytes.saturating_add(input.byte_len()),
            })
    }

    pub(crate) fn plus(self, other: Self) -> Self {
        Self {
            inputs: self.inputs + other.inputs,
            bytes: self.bytes.saturating_add(other.bytes),
        }
    }

    pub(crate) fn is_empty(self) -> bool {
        self.inputs == 0
    }
}

/// The message-log line that tells the user what startup input was lost.
fn discarded_deferred_input_message(
    cause: &str,
    pane_id: PaneId,
    discarded: DeferredInputTally,
) -> String {
    if discarded.is_empty() {
        return cause.to_owned();
    }
    let plural = |count: usize| if count == 1 { "" } else { "s" };
    format!(
        "{cause}; discarded {} queued input{} ({} byte{}) for pane %{}",
        discarded.inputs,
        plural(discarded.inputs),
        discarded.bytes,
        plural(discarded.bytes),
        pane_id.as_u32()
    )
}

impl HandlerState {
    /// Records `cause` in the message log, naming the input it discarded.
    pub(crate) fn report_discarded_deferred_input(
        &mut self,
        cause: &str,
        pane_id: PaneId,
        discarded: DeferredInputTally,
    ) {
        self.add_message(discarded_deferred_input_message(cause, pane_id, discarded));
    }

    /// Removes every pane still starting in a runtime session that is being
    /// destroyed, naming the input each one discards, in pane-id order.
    pub(in crate::pane_terminals) fn remove_session_starting_panes(
        &mut self,
        runtime_session_name: &SessionName,
    ) {
        let Some(panes) = self.starting_panes.remove(runtime_session_name) else {
            return;
        };
        let mut panes = panes.into_iter().collect::<Vec<_>>();
        panes.sort_unstable_by_key(|(pane_id, _)| pane_id.as_u32());
        for (pane_id, removed) in panes {
            let _ = self.report_stopped_starting_pane(pane_id, Some(removed));
        }
    }

    pub(crate) fn queue_starting_pane_input(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        bytes: &[u8],
    ) -> Result<bool, RmuxError> {
        if bytes.is_empty() {
            return Ok(false);
        }
        self.queue_starting_pane_input_entry(
            session_name,
            window_index,
            pane_index,
            DeferredInitialPaneInput::Bytes(bytes.to_vec()),
        )
    }

    pub(crate) fn queue_starting_pane_paste_input(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        bytes: &[u8],
        delimiters: PasteDelimiters,
    ) -> Result<bool, RmuxError> {
        if bytes.is_empty() {
            return Ok(false);
        }
        self.queue_starting_pane_input_entry(
            session_name,
            window_index,
            pane_index,
            DeferredInitialPaneInput::Paste {
                bytes: bytes.to_vec(),
                delimiters,
            },
        )
    }

    pub(crate) fn queue_starting_pane_console_input(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        action: DeferredInitialPaneConsoleInputAction,
        byte_len: usize,
    ) -> Result<bool, RmuxError> {
        self.queue_starting_pane_input_entry(
            session_name,
            window_index,
            pane_index,
            DeferredInitialPaneInput::Console {
                action,
                byte_len: byte_len.max(1),
            },
        )
    }

    fn queue_starting_pane_input_entry(
        &mut self,
        session_name: &SessionName,
        window_index: u32,
        pane_index: u32,
        input: DeferredInitialPaneInput,
    ) -> Result<bool, RmuxError> {
        let Some(pane_id) = self
            .sessions
            .session(session_name)
            .and_then(|session| session.window_at(window_index))
            .and_then(|window| window.pane(pane_index))
            .map(rmux_core::Pane::id)
        else {
            return Ok(false);
        };
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let Some(starting) = self
            .starting_panes
            .get_mut(&runtime_session_name)
            .and_then(|panes| panes.get_mut(&pane_id))
        else {
            return Ok(false);
        };
        let next_len = starting.queued_input_bytes.saturating_add(input.byte_len());
        if next_len > STARTING_PANE_INPUT_MAX_BYTES {
            return Err(RmuxError::Server(format!(
                "pane {}:{window_index}.{pane_index} is still starting and its input queue is full",
                session_name
            )));
        }
        starting.queued_input.push_back(input);
        starting.queued_input_bytes = next_len;
        Ok(true)
    }
}
