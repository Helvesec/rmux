use crate::clock_mode::CLOCK_MODE_NAME;
use crate::copy_mode::{CopyModeRenderSnapshot, CopyModeSummary};

use super::*;

impl PaneTranscript {
    pub(crate) fn copy_mode_state(&self) -> Option<&CopyModeState> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => Some(mode.as_ref()),
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn copy_mode_state_mut(&mut self) -> Option<&mut CopyModeState> {
        match &mut self.mode {
            Some(PaneModeState::Copy(mode)) => Some(mode.as_mut()),
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn set_copy_mode_state(&mut self, state: Option<CopyModeState>) {
        self.mode = state.map(Box::new).map(PaneModeState::Copy);
        self.bump_mode_revision();
    }

    pub(crate) fn copy_mode_summary(&self) -> Option<CopyModeSummary> {
        self.copy_mode_state().map(CopyModeState::summary)
    }

    pub(crate) fn copy_mode_render_screen(&self) -> Option<Screen> {
        self.copy_mode_state().map(CopyModeState::render_screen)
    }

    /// The screen `capture-pane -M` reads while copy mode is active.
    ///
    /// tmux 3.7b captures the copy-mode backing (`window_copy_get_screen`),
    /// with its own history, independent of the scroll position, cursor and
    /// selection of the rendered view. Other modes have no capture screen.
    pub(crate) fn copy_mode_capture_screen(&self) -> Option<&Screen> {
        self.copy_mode_state().map(CopyModeState::backing_screen)
    }

    pub(crate) fn copy_mode_render_snapshot(&self) -> Option<CopyModeRenderSnapshot> {
        self.copy_mode_state().map(|mode| {
            let mut snapshot = mode.render_snapshot();
            // tmux suppresses pane scrollbars from the live base screen's
            // alternate-screen state, even while copy-mode renders its own
            // backing snapshot.
            snapshot.alternate_on = self.presentation_is_alternate();
            snapshot
        })
    }

    #[cfg(feature = "web")]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn copy_mode_render_snapshot_bounded(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> Option<CopyModeRenderSnapshot> {
        self.copy_mode_state().map(|mode| {
            let mut snapshot = mode.render_snapshot_bounded(
                max_string_bytes,
                max_title_stack_bytes,
                max_hyperlink_entry_bytes,
                max_hyperlink_total_bytes,
            );
            snapshot.alternate_on = self.presentation_is_alternate();
            snapshot
        })
    }

    /// Reports whether the copy-mode backing screen's metadata fits the
    /// bounded Web recovery budgets, or `None` when the pane is not in copy
    /// mode.
    #[cfg(feature = "web")]
    pub(crate) fn copy_mode_recovery_metadata_fits(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> Option<bool> {
        self.copy_mode_state().map(|mode| {
            mode.recovery_metadata_fits(
                max_string_bytes,
                max_title_stack_bytes,
                max_hyperlink_entry_bytes,
                max_hyperlink_total_bytes,
            )
        })
    }

    pub(crate) fn clear_copy_mode(&mut self) -> bool {
        match self.mode {
            Some(PaneModeState::Copy(_)) => {
                self.mode = None;
                self.bump_mode_revision();
                true
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => false,
        }
    }

    pub(crate) fn enter_clock_mode(&mut self) -> u64 {
        let generation = self.next_clock_generation;
        self.next_clock_generation = self.next_clock_generation.saturating_add(1);
        self.mode = Some(PaneModeState::Clock(ClockModeState::new(generation)));
        self.bump_mode_revision();
        generation
    }

    pub(crate) fn clock_mode_generation(&self) -> Option<u64> {
        match self.mode {
            Some(PaneModeState::Clock(mode)) => Some(mode.generation()),
            Some(PaneModeState::Copy(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn clear_clock_mode(&mut self) -> bool {
        match self.mode {
            Some(PaneModeState::Clock(_)) => {
                self.mode = None;
                self.bump_mode_revision();
                true
            }
            Some(PaneModeState::Copy(_) | PaneModeState::ModeTree { .. }) | None => false,
        }
    }

    pub(crate) fn enter_mode_tree(&mut self, mode_name: &'static str) -> bool {
        if matches!(
            self.mode.as_ref(),
            Some(PaneModeState::ModeTree { name, .. }) if *name == mode_name
        ) {
            return false;
        }
        let previous = match self.mode.take() {
            Some(PaneModeState::ModeTree { previous, .. }) => previous,
            previous => previous.map(Box::new),
        };
        self.mode = Some(PaneModeState::ModeTree {
            name: mode_name,
            previous,
        });
        self.bump_mode_revision();
        true
    }

    pub(crate) fn clear_mode_tree(&mut self) -> bool {
        match self.mode.take() {
            Some(PaneModeState::ModeTree { previous, .. }) => {
                self.mode = previous.map(|previous| *previous);
                self.bump_mode_revision();
                true
            }
            mode @ (Some(PaneModeState::Copy(_) | PaneModeState::Clock(_)) | None) => {
                self.mode = mode;
                false
            }
        }
    }

    pub(crate) fn pane_in_mode(&self) -> bool {
        self.mode.is_some()
    }

    /// Which mode is on top, for the caller that has to leave them all.
    ///
    /// `copy-mode -q` addresses a pane and not one kind of mode
    /// (`cmd-copy-mode.c:69-71`), so it needs to know what to pop without
    /// reading the name a format would print.
    pub(crate) fn pane_mode_kind(&self) -> Option<PaneModeKind> {
        match &self.mode {
            Some(PaneModeState::Copy(_)) => Some(PaneModeKind::Copy),
            Some(PaneModeState::Clock(_)) => Some(PaneModeKind::Clock),
            Some(PaneModeState::ModeTree { .. }) => Some(PaneModeKind::ModeTree),
            None => None,
        }
    }

    pub(crate) const fn pane_mode_revision(&self) -> u64 {
        self.mode_revision
    }

    fn bump_mode_revision(&mut self) {
        self.mode_revision = self.mode_revision.saturating_add(1);
        // The source clears the flag only where the LAST mode goes; a mode that
        // gives way to the one it was stacked on keeps whatever was unseen.
        if self.mode.is_none() {
            self.unseen_changes = false;
        }
    }

    /// Whether output arrived while this pane was in a mode.
    pub(crate) const fn unseen_changes(&self) -> bool {
        self.unseen_changes
    }

    pub(crate) fn pane_mode_name(&self) -> Option<&'static str> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => Some(if mode.view_mode() {
                "view-mode"
            } else {
                "copy-mode"
            }),
            Some(PaneModeState::Clock(_)) => Some(CLOCK_MODE_NAME),
            Some(PaneModeState::ModeTree { name, .. }) => Some(*name),
            None => None,
        }
    }
}
