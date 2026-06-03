use rmux_core::{GridRenderOptions, PaneId, ScreenCaptureRange};
use rmux_proto::SessionName;

use super::HandlerState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneScrollbackView {
    pub(crate) history_size: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) alternate_on: bool,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
}

impl HandlerState {
    pub(crate) fn pane_scrollback_view(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        scroll_offset: usize,
    ) -> Option<PaneScrollbackView> {
        let window_index = self
            .sessions
            .session(session_name)?
            .window_index_for_pane_id(pane_id)?;
        let runtime_session_name = self.runtime_session_name_for_window(session_name, window_index);
        let transcript = self.transcripts.get(&runtime_session_name)?.get(&pane_id)?;
        let transcript = transcript
            .lock()
            .expect("pane transcript mutex must not be poisoned");
        let screen = transcript.clone_screen();
        let history_size = screen.history_size();
        let alternate_on = screen.is_alternate();
        let scroll_offset = if alternate_on {
            0
        } else {
            scroll_offset.min(history_size)
        };
        let ansi_lines = if scroll_offset == 0 {
            Vec::new()
        } else {
            let top_line = history_size.saturating_sub(scroll_offset);
            let (cursor_x, _) = screen.cursor_position();
            screen
                .clone_viewport(top_line, cursor_x, screen.cursor_absolute_y())
                .capture_transcript_lines_independent(
                    ScreenCaptureRange::default(),
                    GridRenderOptions {
                        with_sequences: true,
                        trim_spaces: false,
                        ..GridRenderOptions::default()
                    },
                )
        };

        Some(PaneScrollbackView {
            history_size,
            scroll_offset,
            alternate_on,
            ansi_lines,
        })
    }
}
