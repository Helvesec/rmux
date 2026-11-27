//! Revision-reusing snapshots from one immutable presentation selection.
use super::*;
#[cfg(test)]
use crate::pane_transcript::PaneTranscript;
use crate::pane_transcript::PresentationRead;

impl PaneRenderSnapshot {
    #[cfg(test)]
    pub(crate) fn capture_unstyled_transcript_reusing(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
        transcript: &PaneTranscript,
        previous: Option<&Self>,
    ) -> Option<Self> {
        Self::capture_presentation_with_inputs(
            &PaneRenderInputs::resolve(session, options, pane)?,
            &transcript.read_presentation(),
            previous,
        )
    }

    pub(crate) fn capture_presentation_with_inputs(
        inputs: &PaneRenderInputs,
        presentation: &PresentationRead<'_>,
        previous: Option<&Self>,
    ) -> Option<Self> {
        let default_style = &inputs.default_style;
        if !inputs.palette.is_empty() {
            return None;
        }
        let state = presentation.render_state();
        if state.has_selected_cells {
            return None;
        }

        let pane_projection = inputs.projection.pane_projection_with_config(
            inputs.pane_id,
            inputs.pane_index,
            &inputs.scrollbar,
            ProjectedPaneScrollbarContext {
                history_size: presentation.screen().history_size(),
                alternate_on: presentation.screen().is_alternate(),
                copy_mode_scroll_position: None,
            },
        )?;
        if pane_projection.spans().is_empty() && pane_projection.scrollbar().is_none() {
            return None;
        }
        let pane_geometry = pane_projection
            .terminal_geometry()
            .unwrap_or_else(|| rmux_core::PaneGeometry::new(0, 0, 0, 0));

        let utf8 = &inputs.utf8;
        let source_row_start = pane_projection.source_row_start();
        let source_width = pane_projection.source_width();
        let row_count = pane_projection.row_count();
        let previous = previous.filter(|previous| {
            previous.x == pane_geometry.x()
                && previous.y == pane_geometry.y()
                && previous.rows == pane_geometry.rows()
                && previous.cols == pane_geometry.cols()
                && previous.source_row_start == source_row_start
                && previous.source_width == source_width
                && previous.row_spans == pane_projection.spans()
        });
        let changes = presentation.capture_projected_line_changes(
            source_row_start,
            row_count,
            GridRenderOptions {
                with_sequences: true,
                include_empty_cells: true,
                trim_spaces: false,
                ..GridRenderOptions::default()
            },
            source_width,
            previous.map(|snapshot| snapshot.line_revisions.as_slice()),
            default_style.as_ref(),
        );
        let mut lines = Vec::with_capacity(changes.len());
        let mut line_revisions = Vec::with_capacity(changes.len());
        for (row, change) in changes.into_iter().enumerate() {
            line_revisions.push(change.revision);
            if let Some(line) = change.rendered {
                let tail_start = change.tail_start.unwrap_or(0);
                lines.push(Arc::new(truncate_rendered_pane_row(
                    &line,
                    usize::from(tail_start),
                    utf8,
                )));
            } else if let Some(previous_line) = change
                .previous_row
                .and_then(|row| previous.and_then(|snapshot| snapshot.lines.get(row)))
            {
                lines.push(Arc::clone(previous_line));
            } else if let Some(previous_line) =
                previous.and_then(|snapshot| snapshot.lines.get(row))
            {
                lines.push(Arc::clone(previous_line));
            } else {
                lines.push(Arc::new(RenderedPaneRow::new(Vec::new(), 0)));
            }
        }

        let cursor = projected_cursor_state(&pane_projection, state.cursor_position);

        Some(Self {
            x: pane_geometry.x(),
            y: pane_geometry.y(),
            rows: pane_geometry.rows(),
            cols: pane_geometry.cols(),
            terminal_cols: inputs.projection.geometry().terminal_size.cols,
            terminal_rows: inputs.projection.geometry().terminal_size.rows,
            source_row_start,
            source_width,
            row_spans: pane_projection.spans().to_vec(),
            utf8: utf8.clone(),
            lines,
            cursor: cursor.bytes,
            cursor_row: cursor.row,
            cursor_col: cursor.col,
            cursor_style: resolve_cursor_style_state(
                state.cursor_style,
                state.mode,
                inputs.cursor_default_style,
            ),
            title: state.title,
            path: state.path,
            mode: state.mode,
            line_revisions,
            scrollbar: pane_projection.scrollbar().cloned(),
        })
    }
}
