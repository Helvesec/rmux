use std::time::SystemTime;

use rmux_core::PaneId;
use rmux_proto::PaneTarget;

use super::HandlerState;

/// Attribution of a captured write at the committed focus producer, after
/// its pane identity and generation have been checked by the normal writer.
#[derive(Clone, Debug)]
pub(crate) struct CommittedFocusInputForTest {
    pub(crate) at: SystemTime,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: u64,
    pub(crate) bytes: Vec<u8>,
    offset: usize,
}

impl HandlerState {
    pub(crate) fn start_pane_input_capture_for_test(&self, target: &PaneTarget) {
        self.pane_input_captures
            .lock()
            .expect("pane input capture mutex")
            .insert(target.to_string(), Vec::new());
        self.pane_focus_input_captures
            .lock()
            .expect("focus capture mutex")
            .remove(&target.to_string());
    }

    pub(crate) fn append_pane_input_capture_for_test(
        &self,
        target: &PaneTarget,
        bytes: &[u8],
    ) -> bool {
        let mut captures = self
            .pane_input_captures
            .lock()
            .expect("pane input capture mutex");
        let Some(captured) = captures.get_mut(&target.to_string()) else {
            return false;
        };
        captured.extend_from_slice(bytes);
        true
    }

    pub(crate) fn pane_input_capture_for_test(&self, target: &PaneTarget) -> Option<Vec<u8>> {
        self.pane_input_captures
            .lock()
            .expect("pane input capture mutex")
            .get(&target.to_string())
            .cloned()
    }

    pub(crate) fn attribute_captured_focus_input_for_test(
        &self,
        target: &PaneTarget,
        pane_id: PaneId,
        generation: u64,
        bytes: &[u8],
    ) {
        let captures = self
            .pane_input_captures
            .lock()
            .expect("pane input capture mutex");
        let captured = captures
            .get(&target.to_string())
            .expect("focus write was captured");
        let offset = captured
            .len()
            .checked_sub(bytes.len())
            .expect("captured focus span");
        assert_eq!(&captured[offset..], bytes);
        self.pane_focus_input_captures
            .lock()
            .expect("focus capture mutex")
            .entry(target.to_string())
            .or_default()
            .push(CommittedFocusInputForTest {
                at: SystemTime::now(),
                pane_id,
                generation,
                bytes: bytes.to_vec(),
                offset,
            });
    }

    pub(crate) fn pane_focus_input_capture_for_test(
        &self,
        target: &PaneTarget,
    ) -> Vec<CommittedFocusInputForTest> {
        self.pane_focus_input_captures
            .lock()
            .expect("focus capture mutex")
            .get(&target.to_string())
            .cloned()
            .unwrap_or_default()
    }

    /// Preserve the complete raw capture; select only spans that did not
    /// originate from the committed focus producer. Callers must check the
    /// attributed spans separately, including the destination generation.
    pub(crate) fn pane_non_focus_input_capture_for_test(
        &self,
        target: &PaneTarget,
    ) -> Option<Vec<u8>> {
        let bytes = self.pane_input_capture_for_test(target)?;
        let mut other = Vec::new();
        let mut cursor = 0;
        for focus in self.pane_focus_input_capture_for_test(target) {
            assert!(focus.offset >= cursor);
            other.extend_from_slice(&bytes[cursor..focus.offset]);
            cursor = focus.offset + focus.bytes.len();
            assert_eq!(&bytes[focus.offset..cursor], focus.bytes);
        }
        other.extend_from_slice(&bytes[cursor..]);
        Some(other)
    }
}
