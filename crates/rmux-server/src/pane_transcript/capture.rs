use rmux_core::{input::OscColourSlot, GridRenderOptions, LineFormatCapture, ScreenCaptureRange};

#[cfg(test)]
use super::presentation::PaneTranscriptRenderState;
use super::*;

impl PaneTranscript {
    pub(crate) fn capture_main(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<u8> {
        self.terminal.screen().capture_transcript(range, options)
    }

    pub(crate) fn capture_main_hyperlinks(
        &self,
        range: ScreenCaptureRange,
        join_wrapped: bool,
    ) -> Vec<u8> {
        self.terminal
            .screen()
            .capture_hyperlinks(range, join_wrapped)
    }

    pub(crate) fn capture_main_line_format(&self, range: ScreenCaptureRange) -> LineFormatCapture {
        self.terminal.screen().capture_line_format(range)
    }

    #[cfg(test)]
    pub(crate) fn render_state(&self) -> PaneTranscriptRenderState {
        self.read_presentation().render_state()
    }

    pub(crate) fn plain_output_forwarding_safe(&self) -> bool {
        self.terminal.plain_output_forwarding_safe()
    }

    pub(crate) fn capture_saved(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        self.terminal
            .screen()
            .capture_saved_transcript(range, options)
    }

    pub(crate) fn capture_saved_hyperlinks(
        &self,
        range: ScreenCaptureRange,
        join_wrapped: bool,
    ) -> Option<Vec<u8>> {
        self.terminal
            .screen()
            .capture_saved_hyperlinks(range, join_wrapped)
    }

    pub(crate) fn capture_saved_line_format(
        &self,
        range: ScreenCaptureRange,
    ) -> Option<LineFormatCapture> {
        self.terminal.screen().capture_saved_line_format(range)
    }

    pub(crate) fn capture_copy_mode(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => {
                Some(mode.render_screen().capture_transcript(range, options))
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn capture_copy_mode_hyperlinks(
        &self,
        range: ScreenCaptureRange,
        join_wrapped: bool,
    ) -> Option<Vec<u8>> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => {
                Some(mode.render_screen().capture_hyperlinks(range, join_wrapped))
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn capture_copy_mode_line_format(
        &self,
        range: ScreenCaptureRange,
    ) -> Option<LineFormatCapture> {
        match &self.mode {
            Some(PaneModeState::Copy(mode)) => {
                Some(mode.render_screen().capture_line_format(range))
            }
            Some(PaneModeState::Clock(_) | PaneModeState::ModeTree { .. }) | None => None,
        }
    }

    pub(crate) fn pending_bytes(&self) -> Vec<u8> {
        self.terminal.pending_bytes()
    }

    pub(crate) fn pending_bytes_ref(&self) -> &[u8] {
        self.terminal.pending_bytes_ref()
    }

    pub(crate) fn active_cell_state_ansi_bounded(
        &self,
        max_hyperlink_bytes: usize,
    ) -> (Vec<u8>, bool) {
        self.terminal
            .active_cell_state_ansi_bounded(max_hyperlink_bytes)
    }

    pub(crate) fn saved_cell_state_ansi_bounded(
        &self,
        max_hyperlink_bytes: usize,
    ) -> (Vec<u8>, bool) {
        self.terminal
            .saved_cell_state_ansi_bounded(max_hyperlink_bytes)
    }

    pub(crate) fn saved_cursor_state(&self) -> (u32, u32, bool) {
        self.terminal.saved_cursor_state()
    }

    pub(crate) fn recovery_parser_state_ansi(&self) -> Vec<u8> {
        self.terminal.recovery_parser_state_ansi()
    }

    pub(crate) fn dynamic_colour(&self, slot: OscColourSlot) -> Option<&str> {
        self.terminal.dynamic_colour(slot)
    }
}
