use super::super::pending_escape::PendingEscapeFlush;
use super::super::types::{AttachTarget, OpenAttachTarget};

pub(in crate::pane_io) struct PendingAttachInputState<'a> {
    bytes: &'a mut Vec<u8>,
    escape_flush: &'a mut PendingEscapeFlush,
}

impl<'a> PendingAttachInputState<'a> {
    pub(in crate::pane_io) fn new(
        bytes: &'a mut Vec<u8>,
        escape_flush: &'a mut PendingEscapeFlush,
    ) -> Self {
        Self {
            bytes,
            escape_flush,
        }
    }

    pub(in crate::pane_io) fn clear_if_pane_source_changed(
        &mut self,
        current_target: &OpenAttachTarget,
        next_target: &AttachTarget,
    ) {
        let same_pane_source = current_target
            .pane_output
            .as_ref()
            .is_some_and(|current_output| {
                current_output.shares_pane_source_with(&next_target.pane_output)
            });
        if same_pane_source {
            return;
        }

        self.clear();
    }

    pub(in crate::pane_io) fn parts_mut(&mut self) -> (&mut Vec<u8>, &mut PendingEscapeFlush) {
        (self.bytes, self.escape_flush)
    }

    pub(in crate::pane_io) fn clear(&mut self) {
        self.bytes.clear();
        self.escape_flush.clear();
    }
}
