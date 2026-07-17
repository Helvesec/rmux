use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use rmux_core::alternate_screen_exit_sequence;

pub(super) const ALT_SCREEN_EXIT_FALLBACK: &[u8] = b"\x1b[?1049l";
#[cfg(test)]
pub(super) const DETACHED_BANNER_PREFIX: &[u8] = b"[detached (from session ";
#[cfg(test)]
pub(super) const EXITED_BANNER: &[u8] = b"[exited]\r\n";
const STACK_STOP_SCAN_BYTES: usize = 128;

const ATTACH_SCREEN_STOPPED_BIT: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AttachStopGeneration(u64);

#[derive(Clone, Debug)]
pub(super) struct AttachScreenTracker {
    state: Arc<AtomicU64>,
}

impl Default for AttachScreenTracker {
    fn default() -> Self {
        Self {
            state: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl AttachScreenTracker {
    pub(super) fn mark_stopped(&self) -> AttachStopGeneration {
        let state = self
            .state
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_add(2) | ATTACH_SCREEN_STOPPED_BIT)
            })
            .expect("attach screen generation update is infallible");
        AttachStopGeneration(state.saturating_add(2) | ATTACH_SCREEN_STOPPED_BIT)
    }

    pub(super) fn current_stop_generation(&self) -> Option<AttachStopGeneration> {
        let state = self.state.load(Ordering::SeqCst);
        (state & ATTACH_SCREEN_STOPPED_BIT != 0).then_some(AttachStopGeneration(state))
    }

    pub(super) fn rearm_if_current(&self, generation: AttachStopGeneration) -> bool {
        if generation.0 == u64::MAX {
            return false;
        }
        self.state
            .compare_exchange(
                generation.0,
                generation.0 & !ATTACH_SCREEN_STOPPED_BIT,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    pub(super) fn was_stopped(&self) -> bool {
        self.state.load(Ordering::SeqCst) & ATTACH_SCREEN_STOPPED_BIT != 0
    }
}

#[derive(Debug)]
pub(super) struct AttachStopDetector {
    tracker: AttachScreenTracker,
    marker: Vec<u8>,
    tail: Vec<u8>,
}

impl AttachStopDetector {
    pub(super) fn new(tracker: AttachScreenTracker) -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let marker = alternate_screen_exit_sequence(&term).to_vec();
        let tail_len = stop_marker_tail_len(&marker);
        Self {
            tracker,
            marker,
            tail: Vec::with_capacity(tail_len),
        }
    }

    #[cfg(windows)]
    pub(super) fn current_stop_generation(&self) -> Option<AttachStopGeneration> {
        self.tracker.current_stop_generation()
    }

    pub(super) fn observe(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }

        if !contains_stop_marker_start(bytes)
            && (self.tail.is_empty() || !contains_stop_marker_start(&self.tail))
        {
            self.update_tail(bytes);
            return;
        }

        if contains_stop_marker(bytes, &self.marker) {
            self.tracker.mark_stopped();
            self.tail.clear();
            return;
        }

        if self.tail.is_empty() {
            self.update_tail(bytes);
            return;
        }

        let combined_len = self.tail.len() + bytes.len();
        if combined_len <= STACK_STOP_SCAN_BYTES {
            let mut combined = [0_u8; STACK_STOP_SCAN_BYTES];
            combined[..self.tail.len()].copy_from_slice(&self.tail);
            combined[self.tail.len()..combined_len].copy_from_slice(bytes);
            let combined = &combined[..combined_len];
            if contains_stop_marker(combined, &self.marker) {
                self.tracker.mark_stopped();
                self.tail.clear();
                return;
            }
            self.update_tail(combined);
            return;
        }

        let mut combined = Vec::with_capacity(combined_len);
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        if contains_stop_marker(&combined, &self.marker) {
            self.tracker.mark_stopped();
            self.tail.clear();
            return;
        }

        self.update_tail(&combined);
    }

    fn update_tail(&mut self, bytes: &[u8]) {
        let tail_len = stop_marker_tail_len(&self.marker);
        self.tail.clear();
        if tail_len == 0 {
            return;
        }
        let start = bytes.len().saturating_sub(tail_len);
        self.tail.extend_from_slice(&bytes[start..]);
    }
}

fn stop_marker_tail_len(marker: &[u8]) -> usize {
    [marker.len(), ALT_SCREEN_EXIT_FALLBACK.len()]
        .into_iter()
        .max()
        .unwrap_or(0)
        .saturating_sub(1)
}

pub(super) fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn contains_stop_marker(bytes: &[u8], marker: &[u8]) -> bool {
    contains_subslice(bytes, marker) || contains_subslice(bytes, ALT_SCREEN_EXIT_FALLBACK)
}

pub(super) fn contains_stop_marker_start(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|window| window == b"\x1b[")
        || bytes.last().is_some_and(|byte| *byte == b'\x1b')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_len_covers_all_stop_markers() {
        let marker = b"\x1b[?1049l";
        let tail_len = stop_marker_tail_len(marker);

        for needle in [marker.as_slice(), ALT_SCREEN_EXIT_FALLBACK] {
            assert!(
                tail_len >= needle.len().saturating_sub(1),
                "tail length {tail_len} should cover marker length {}",
                needle.len()
            );
        }
    }

    #[test]
    fn literal_detach_and_exit_banners_do_not_stop_attach() {
        let tracker = AttachScreenTracker::default();
        let mut detector = AttachStopDetector::new(tracker.clone());
        detector.observe(DETACHED_BANNER_PREFIX);
        detector.observe(EXITED_BANNER);
        assert!(
            !tracker.was_stopped(),
            "ordinary pane bytes must never be treated as lifecycle authority"
        );
    }

    #[test]
    fn detector_marks_alt_screen_exit_without_closing_attach() {
        let tracker = AttachScreenTracker::default();
        let mut detector = AttachStopDetector::new(tracker.clone());

        detector.observe(ALT_SCREEN_EXIT_FALLBACK);

        assert!(tracker.was_stopped());
    }

    #[test]
    fn stopped_screen_can_be_rearmed_for_resumed_attach() {
        let tracker = AttachScreenTracker::default();

        let generation = tracker.mark_stopped();
        assert!(tracker.was_stopped());

        assert!(tracker.rearm_if_current(generation));
        assert!(
            !tracker.was_stopped(),
            "a resumed attach must treat a later EOF as abnormal"
        );
    }

    #[test]
    fn later_stop_prevents_stale_resume_from_rearming_attach() {
        let tracker = AttachScreenTracker::default();
        let lock_prelude = tracker.mark_stopped();

        let final_stop = tracker.mark_stopped();

        assert_ne!(lock_prelude, final_stop);
        assert!(
            !tracker.rearm_if_current(lock_prelude),
            "a completed lock must not erase a later detach or exit stop"
        );
        assert!(tracker.was_stopped());
        assert_eq!(tracker.current_stop_generation(), Some(final_stop));
    }

    #[test]
    fn rearmed_detector_does_not_reuse_stale_split_marker_prefix() {
        let tracker = AttachScreenTracker::default();
        let mut detector = AttachStopDetector::new(tracker.clone());
        let split = ALT_SCREEN_EXIT_FALLBACK.len() - 2;

        detector.observe(&ALT_SCREEN_EXIT_FALLBACK[..split]);
        detector.observe(&ALT_SCREEN_EXIT_FALLBACK[split..]);
        assert!(tracker.was_stopped());

        let generation = tracker
            .current_stop_generation()
            .expect("detector should publish a stop generation");
        assert!(tracker.rearm_if_current(generation));
        detector.observe(&ALT_SCREEN_EXIT_FALLBACK[split..]);
        assert!(
            !tracker.was_stopped(),
            "rearm must start a fresh stop-marker observation window"
        );
    }

    #[test]
    fn stop_marker_start_ignores_common_log_brackets() {
        assert!(!contains_stop_marker_start(b"[INFO] still running"));
        assert!(contains_stop_marker_start(b"\x1b[?1049l"));
        assert!(contains_stop_marker_start(b"partial \x1b"));
    }
}
