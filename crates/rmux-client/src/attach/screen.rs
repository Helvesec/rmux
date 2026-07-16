use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmux_core::alternate_screen_exit_sequence;

pub(super) const ALT_SCREEN_EXIT_FALLBACK: &[u8] = b"\x1b[?1049l";
#[cfg(test)]
pub(super) const DETACHED_BANNER_PREFIX: &[u8] = b"[detached (from session ";
#[cfg(test)]
pub(super) const EXITED_BANNER: &[u8] = b"[exited]\r\n";
const STACK_STOP_SCAN_BYTES: usize = 128;

#[derive(Clone, Debug, Default)]
pub(super) struct AttachScreenTracker {
    stopped: Arc<AtomicBool>,
}

impl AttachScreenTracker {
    pub(super) fn mark_stopped(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }

    pub(super) fn was_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
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
    fn stop_marker_start_ignores_common_log_brackets() {
        assert!(!contains_stop_marker_start(b"[INFO] still running"));
        assert!(contains_stop_marker_start(b"\x1b[?1049l"));
        assert!(contains_stop_marker_start(b"partial \x1b"));
    }
}
