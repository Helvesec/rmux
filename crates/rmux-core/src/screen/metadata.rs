//! Terminal-controlled metadata of [`Screen`]: titles, window name, path and program status.

use super::{Screen, TITLE_STACK_MAX};

impl Screen {
    /// Returns the current screen title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Returns the title stack from oldest to newest.
    #[must_use]
    pub fn title_stack(&self) -> &[String] {
        &self.title_stack
    }

    /// Maximum title stack depth accepted by the terminal model.
    #[must_use]
    pub const fn title_stack_limit() -> usize {
        TITLE_STACK_MAX
    }

    /// Sets the current screen title.
    pub fn set_title(&mut self, title: impl Into<String>) {
        let title = title.into();
        if self.title != title {
            self.title = title;
            self.bump_metadata_revision();
        }
    }

    /// Returns the revision of terminal-controlled metadata.
    #[must_use]
    pub const fn metadata_revision(&self) -> u64 {
        self.metadata_revision
    }

    pub(super) fn bump_metadata_revision(&mut self) {
        self.metadata_revision = self.metadata_revision.saturating_add(1);
    }

    /// Drains the window-rename requests accepted since the last drain.
    ///
    /// Every accepted request is kept, in the order the pane emitted it: an
    /// empty name and a name identical to the previous one are requests in
    /// their own right, because tmux's effects and its `window-renamed`
    /// notification follow the request, not a difference of stored name. The
    /// owner drains after every feed, exactly like the parser replies.
    pub fn take_window_rename_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.window_rename_requests)
    }

    pub(crate) fn record_window_rename_request(&mut self, name: &str) {
        self.window_rename_requests.push(name.to_owned());
    }

    /// Terminal-local OSC 7501 records, unaffected by alternate-screen switching.
    #[must_use]
    pub fn program_status(&self) -> &crate::ProgramStatus {
        &self.program_status
    }

    /// Expire active/idle reports when the attached process exits or a prompt starts.
    pub fn expire_program_status(&mut self) {
        self.program_status.expire_active();
    }

    /// Returns the most recent OSC 7 path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}
