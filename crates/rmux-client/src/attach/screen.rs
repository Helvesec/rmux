use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmux_core::alternate_screen_exit_sequence;

pub(super) const ALT_SCREEN_EXIT_FALLBACK: &[u8] = b"\x1b[?1049l";
pub(super) const DETACHED_BANNER_PREFIX: &[u8] = b"[detached (from session ";
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct AttachStopObservation {
    attach_done: bool,
}

impl AttachStopObservation {
    #[must_use]
    #[cfg_attr(windows, allow(dead_code))]
    pub(super) const fn attach_done(self) -> bool {
        self.attach_done
    }
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

    pub(super) fn observe(&mut self, bytes: &[u8]) -> AttachStopObservation {
        if bytes.is_empty() {
            return AttachStopObservation::default();
        }

        if !contains_stop_marker_start(bytes)
            && (self.tail.is_empty() || !contains_stop_marker_start(&self.tail))
        {
            self.update_tail(bytes);
            return AttachStopObservation::default();
        }

        let marker = find_stop_marker(bytes, &self.marker);
        if marker != StopMarker::None {
            return self.observe_marker(marker);
        }

        if self.tail.is_empty() {
            self.update_tail(bytes);
            return AttachStopObservation::default();
        }

        let combined_len = self.tail.len() + bytes.len();
        if combined_len <= STACK_STOP_SCAN_BYTES {
            let mut combined = [0_u8; STACK_STOP_SCAN_BYTES];
            combined[..self.tail.len()].copy_from_slice(&self.tail);
            combined[self.tail.len()..combined_len].copy_from_slice(bytes);
            let combined = &combined[..combined_len];
            let marker = find_stop_marker(combined, &self.marker);
            if marker != StopMarker::None {
                return self.observe_marker(marker);
            }
            self.update_tail(combined);
            return AttachStopObservation::default();
        }

        let mut combined = Vec::with_capacity(combined_len);
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        let marker = find_stop_marker(&combined, &self.marker);
        if marker != StopMarker::None {
            return self.observe_marker(marker);
        }

        self.update_tail(&combined);
        AttachStopObservation::default()
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

    fn observe_marker(&self, marker: StopMarker) -> AttachStopObservation {
        self.tracker.mark_stopped();
        AttachStopObservation {
            attach_done: marker == StopMarker::AttachDone,
        }
    }
}

fn stop_marker_tail_len(marker: &[u8]) -> usize {
    [
        marker.len(),
        ALT_SCREEN_EXIT_FALLBACK.len(),
        DETACHED_BANNER_PREFIX.len(),
        EXITED_BANNER.len(),
    ]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopMarker {
    None,
    ScreenStop,
    AttachDone,
}

fn find_stop_marker(bytes: &[u8], marker: &[u8]) -> StopMarker {
    if contains_subslice(bytes, DETACHED_BANNER_PREFIX) || contains_subslice(bytes, EXITED_BANNER) {
        return StopMarker::AttachDone;
    }
    if contains_subslice(bytes, marker) || contains_subslice(bytes, ALT_SCREEN_EXIT_FALLBACK) {
        return StopMarker::ScreenStop;
    }
    StopMarker::None
}

pub(super) fn contains_stop_marker_start(bytes: &[u8]) -> bool {
    bytes
        .windows(2)
        .any(|window| matches!(window, b"\x1b[" | b"[d" | b"[e"))
        || bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\x1b' | b'['))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_len_covers_all_stop_markers() {
        let marker = b"\x1b[?1049l";
        let tail_len = stop_marker_tail_len(marker);

        for needle in [
            marker.as_slice(),
            ALT_SCREEN_EXIT_FALLBACK,
            DETACHED_BANNER_PREFIX,
            EXITED_BANNER,
        ] {
            assert!(
                tail_len >= needle.len().saturating_sub(1),
                "tail length {tail_len} should cover marker length {}",
                needle.len()
            );
        }
    }

    #[test]
    fn detector_marks_stopped_when_detached_banner_is_split_across_reads() {
        let tracker = AttachScreenTracker::default();
        let mut detector = AttachStopDetector::new(tracker.clone());
        let split = 12;

        assert!(!detector
            .observe(&DETACHED_BANNER_PREFIX[..split])
            .attach_done());
        assert!(!tracker.was_stopped());

        assert!(detector
            .observe(&DETACHED_BANNER_PREFIX[split..])
            .attach_done());
        assert!(tracker.was_stopped());
    }

    #[test]
    fn detector_marks_alt_screen_exit_without_closing_attach() {
        let tracker = AttachScreenTracker::default();
        let mut detector = AttachStopDetector::new(tracker.clone());

        let observation = detector.observe(ALT_SCREEN_EXIT_FALLBACK);

        assert!(tracker.was_stopped());
        assert!(!observation.attach_done());
    }

    #[test]
    fn stop_marker_start_ignores_common_log_brackets() {
        assert!(!contains_stop_marker_start(b"[INFO] still running"));
        assert!(contains_stop_marker_start(b"\x1b[?1049l"));
        assert!(contains_stop_marker_start(b"[detached"));
        assert!(contains_stop_marker_start(b"partial \x1b"));
    }
}
