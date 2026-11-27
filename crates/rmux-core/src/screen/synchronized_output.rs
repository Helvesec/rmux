//! Synchronized output (DEC mode 2026): the frozen presentation snapshot
//! shown while an application batches an update.

use super::Screen;
use crate::input::mode;

impl Screen {
    pub(crate) fn presentation_screen(&self) -> &Self {
        self.synchronized_output_snapshot.as_deref().unwrap_or(self)
    }

    pub(crate) fn take_synchronized_output_started(&mut self) -> bool {
        std::mem::take(&mut self.synchronized_output_started)
    }

    pub(crate) fn synchronized_output_active(&self) -> bool {
        self.mode & mode::MODE_SYNC != 0
    }

    pub(crate) fn finish_synchronized_output(&mut self) -> bool {
        self.end_synchronized_output()
    }

    pub(super) fn begin_synchronized_output(&mut self) {
        if self.synchronized_output_active() {
            return;
        }

        // Snapshot at the ScreenWriter transition itself. Bytes after the
        // DECSET in this same parser feed may mutate `self`, but every
        // presentation path keeps seeing this single bounded clone.
        let mut presentation = self.clone();
        presentation.synchronized_output_snapshot = None;
        presentation.mode &= !mode::MODE_SYNC;
        presentation.bell_count = 0;
        presentation.terminal_passthrough.clear();
        presentation.dropped_terminal_passthrough_count = 0;
        self.synchronized_output_snapshot = Some(Box::new(presentation));
        self.synchronized_output_started = true;
        self.mode |= mode::MODE_SYNC;
    }

    pub(super) fn end_synchronized_output(&mut self) -> bool {
        let had_snapshot = self.synchronized_output_snapshot.take().is_some();
        let active = self.synchronized_output_active() || had_snapshot;
        self.synchronized_output_started = false;
        self.mode &= !mode::MODE_SYNC;
        active
    }
}
