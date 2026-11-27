use rmux_core::GridRenderOptions;
use rmux_proto::RmuxError;

use crate::pane_recovery::{
    validate_recovery_geometry, MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
    MAX_RECOVERY_HYPERLINK_TOTAL_BYTES, MAX_RECOVERY_STRING_BYTES, MAX_RECOVERY_TITLE_STACK_BYTES,
};
use crate::web::WEB_RECOVERY_CONTENT_BYTES_MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneScrollbackView {
    pub(crate) history_size: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) alternate_on: bool,
    pub(crate) ansi_lines: Vec<Vec<u8>>,
    pub(crate) metadata_complete: bool,
}

impl PaneScrollbackView {
    pub(crate) fn capture(
        transcript: &crate::pane_transcript::PaneTranscript,
        scroll_offset_for: impl FnOnce(usize, bool) -> usize,
    ) -> Result<Self, RmuxError> {
        let screen = transcript.screen();
        validate_recovery_geometry(screen)?;
        let history_size = screen.history_size();
        let alternate_on = screen.is_alternate();
        let scroll_offset = scroll_offset_for(history_size, alternate_on);
        // While the pane is in copy mode the Web viewer renders the copy-mode
        // backing screen, whose bounded clone drops the same over-budget
        // metadata. Both screens therefore decide this pane's coverage.
        let metadata_complete = screen.recovery_metadata_fits(
            MAX_RECOVERY_STRING_BYTES,
            MAX_RECOVERY_TITLE_STACK_BYTES,
            MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
            MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
        ) && transcript
            .copy_mode_recovery_metadata_fits(
                MAX_RECOVERY_STRING_BYTES,
                MAX_RECOVERY_TITLE_STACK_BYTES,
                MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
                MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
            )
            .unwrap_or(true);
        let (ansi_lines, metadata_complete) = if scroll_offset == 0 {
            (Vec::new(), metadata_complete)
        } else {
            let top_line = history_size.saturating_sub(scroll_offset);
            let renderer = screen.recovery_row_renderer(
                MAX_RECOVERY_HYPERLINK_ENTRY_BYTES,
                MAX_RECOVERY_HYPERLINK_TOTAL_BYTES,
            );
            let options = GridRenderOptions {
                with_sequences: true,
                trim_spaces: false,
                ..GridRenderOptions::default()
            };
            let mut rendered_bytes = 0_usize;
            let mut lines = Vec::with_capacity(usize::from(screen.size().rows));
            for row in top_line..top_line.saturating_add(usize::from(screen.size().rows)) {
                // Each row is repainted at an absolute cursor position by the
                // Web snapshot, so this view needs no soft-wrap linking.
                let Some(rendered) = renderer.active_row(row, options) else {
                    return Err(RmuxError::Server(
                        "terminal row disappeared during Web scrollback capture".to_owned(),
                    ));
                };
                rendered_bytes = rendered_bytes.saturating_add(rendered.bytes.len());
                if rendered_bytes > WEB_RECOVERY_CONTENT_BYTES_MAX {
                    return Err(RmuxError::FrameTooLarge {
                        length: rendered_bytes,
                        maximum: WEB_RECOVERY_CONTENT_BYTES_MAX,
                    });
                }
                lines.push(rendered.bytes);
            }
            (lines, metadata_complete && renderer.metadata_complete())
        };

        Ok(PaneScrollbackView {
            history_size,
            scroll_offset,
            alternate_on,
            ansi_lines,
            metadata_complete,
        })
    }
}
