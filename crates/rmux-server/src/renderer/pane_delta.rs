use std::io::Write as _;
use std::sync::Arc;

use rmux_core::{
    cursor_style_from_option, input::mode, resolve_cursor_style_state, GridRenderOptions,
    OptionStore, Pane, ResolvedCursorStyle, Screen,
};

use crate::pane_scrollbar::PaneScrollbarConfig;
mod diff;
mod plain_output;
mod presentation_capture;

use super::pane_row::{
    append_pane_row_tail, capture_pane_empty_tail_starts_from, crop_rendered_pane_row,
    truncate_rendered_pane_row, RenderedPaneRow,
};
use super::pane_screen::{pane_default_style, pane_selection_overlay_style};
use super::pane_scrollbar::RenderedPaneScrollbar;
use super::projection::{
    ProjectedPaneScrollbarContext, TerminalPaneProjection, TerminalPaneSpan,
    TerminalSceneProjection,
};
use super::{cursor_position_bytes, replace_cursor_position_bytes};

const LINE_REVISION_GENERATION_BITS: u32 = 32;
#[cfg(test)]
const LINE_REVISION_GENERATION_MASK: u64 = (1_u64 << LINE_REVISION_GENERATION_BITS) - 1;
const LINE_REVISION_ID_MASK: u64 = !((1_u64 << LINE_REVISION_GENERATION_BITS) - 1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneRenderDelta {
    Incremental(PaneRenderDeltaFrame),
    RequiresFullRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRenderDeltaFrame {
    frame: Vec<u8>,
    cursor_style: Option<ResolvedCursorStyle>,
}

impl PaneRenderDeltaFrame {
    #[cfg(test)]
    pub(crate) fn new(frame: Vec<u8>, cursor_style: Option<u32>) -> Self {
        Self {
            frame,
            cursor_style: cursor_style.map(Into::into),
        }
    }

    pub(crate) fn new_with_cursor_state(
        frame: Vec<u8>,
        cursor_style: Option<ResolvedCursorStyle>,
    ) -> Self {
        Self {
            frame,
            cursor_style,
        }
    }

    pub(crate) fn frame(&self) -> &[u8] {
        &self.frame
    }

    pub(crate) fn cursor_style(&self) -> Option<u32> {
        self.cursor_style.map(ResolvedCursorStyle::style)
    }

    pub(crate) fn cursor_style_state(&self) -> Option<ResolvedCursorStyle> {
        self.cursor_style
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneRenderSnapshot {
    x: u16,
    y: u16,
    rows: u16,
    cols: u16,
    terminal_cols: u16,
    terminal_rows: u16,
    source_row_start: usize,
    source_width: u16,
    row_spans: Vec<TerminalPaneSpan>,
    utf8: rmux_core::Utf8Config,
    lines: Vec<Arc<RenderedPaneRow>>,
    cursor: Vec<u8>,
    cursor_row: u16,
    cursor_col: u16,
    cursor_style: ResolvedCursorStyle,
    title: String,
    path: String,
    mode: u32,
    line_revisions: Vec<u64>,
    scrollbar: Option<RenderedPaneScrollbar>,
}

/// Immutable inputs retained by a client renderer; no session or window owner.
#[derive(Debug)]
pub(crate) struct PaneRenderInputs {
    projection: TerminalSceneProjection,
    pane_id: rmux_core::PaneId,
    pane_index: u32,
    scrollbar: PaneScrollbarConfig,
    default_style: Option<rmux_core::style::Style>,
    palette: Vec<(u32, i32)>,
    selection_style: Option<String>,
    utf8: rmux_core::Utf8Config,
    cursor_default_style: u32,
}

impl PaneRenderInputs {
    pub(crate) fn resolve(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
    ) -> Option<Self> {
        Some(Self {
            projection: TerminalSceneProjection::resolve(session, options),
            pane_id: pane.id(),
            pane_index: pane.index(),
            scrollbar: PaneScrollbarConfig::resolve(
                options,
                session.name(),
                session.active_window_index(),
                pane.index(),
            ),
            default_style: pane_default_style(session, options, pane),
            palette: super::pane_palette::resolve(session, options, pane),
            selection_style: pane_selection_overlay_style(session, options, pane),
            utf8: rmux_core::Utf8Config::from_options(options),
            cursor_default_style: cursor_style_from_option(options.resolve_for_pane(
                session.name(),
                session.active_window_index(),
                pane.index(),
                rmux_proto::OptionName::CursorStyle,
            )),
        })
    }

    pub(crate) fn is_unstyled(&self) -> bool {
        self.default_style.is_none() && self.palette.is_empty()
    }
}

impl PaneRenderSnapshot {
    pub(crate) const fn cursor_style_state(&self) -> ResolvedCursorStyle {
        self.cursor_style
    }

    fn cursor_visible(&self) -> bool {
        self.mode & mode::MODE_CURSOR != 0 && !self.cursor.is_empty()
    }

    fn cursor_visibility_changed(&self, next: &Self) -> bool {
        self.cursor_visible() != next.cursor_visible()
    }

    fn append_final_cursor_state(&self, frame: &mut Vec<u8>) {
        frame.extend_from_slice(&self.cursor);
        if self.cursor_visible() {
            frame.extend_from_slice(b"\x1b[?25h");
        } else {
            frame.extend_from_slice(b"\x1b[?25l");
        }
    }

    #[cfg(test)]
    pub(crate) fn capture(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
        screen: &Screen,
    ) -> Option<Self> {
        Self::capture_with_inputs(&PaneRenderInputs::resolve(session, options, pane)?, screen)
    }

    pub(crate) fn capture_with_inputs(inputs: &PaneRenderInputs, screen: &Screen) -> Option<Self> {
        let pane_projection = inputs.projection.pane_projection_with_config(
            inputs.pane_id,
            inputs.pane_index,
            &inputs.scrollbar,
            ProjectedPaneScrollbarContext {
                history_size: screen.history_size(),
                alternate_on: screen.is_alternate(),
                copy_mode_scroll_position: None,
            },
        )?;
        if pane_projection.spans().is_empty() && pane_projection.scrollbar().is_none() {
            return None;
        }
        // A clipped body may have no terminal geometry while its decoration is visible.
        let pane_geometry = pane_projection
            .terminal_geometry()
            .unwrap_or_else(|| rmux_core::PaneGeometry::new(0, 0, 0, 0));

        let utf8 = &inputs.utf8;
        let source_row_start = pane_projection.source_row_start();
        let source_width = pane_projection.source_width();
        let row_count = pane_projection.row_count();
        let lines = capture_visible_pane_lines(
            inputs,
            screen,
            source_row_start,
            row_count,
            source_width,
            utf8,
        )
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<_>>();
        let line_revisions = (0..row_count)
            .map(|row| {
                screen
                    .visible_line_revision(source_row_start.saturating_add(row))
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();

        let cursor = projected_cursor_state(&pane_projection, screen.cursor_position());

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
                screen.cursor_style(),
                screen.mode(),
                inputs.cursor_default_style,
            ),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode: screen.mode(),
            line_revisions,
            scrollbar: pane_projection.scrollbar().cloned(),
        })
    }

    pub(crate) fn full_frame(&self) -> Vec<u8> {
        let mut frame = Vec::with_capacity(self.frame_capacity_hint());
        frame.extend_from_slice(b"\x1b[s\x1b[?25l\x1b[0m");
        for span in &self.row_spans {
            append_projected_snapshot_span(&mut frame, self, span, self.lines.get(span.line_index));
        }
        if let Some(scrollbar) = self.scrollbar.as_ref() {
            frame.extend_from_slice(scrollbar.frame().as_slice());
        }
        frame.extend_from_slice(b"\x1b[0m\x1b[u");
        self.append_final_cursor_state(&mut frame);
        frame
    }

    fn frame_capacity_hint(&self) -> usize {
        let line_bytes = self
            .lines
            .iter()
            .map(|line| line.bytes().len())
            .sum::<usize>();
        line_bytes
            .saturating_add(usize::from(self.rows).saturating_mul(16))
            .saturating_add(
                self.scrollbar
                    .as_ref()
                    .map_or(0, |scrollbar| scrollbar.frame().len()),
            )
            .saturating_add(32)
    }
}

struct ProjectedCursor {
    bytes: Vec<u8>,
    row: u16,
    col: u16,
}

fn projected_cursor_state(
    projection: &TerminalPaneProjection,
    cursor_position: (u32, u32),
) -> ProjectedCursor {
    let (cursor_x, cursor_y) = cursor_position;
    let cursor_x = cursor_x.min(u32::from(projection.source_width().saturating_sub(1))) as u16;
    if let Some((col, row)) = projection.visible_terminal_cursor(cursor_x, cursor_y) {
        return ProjectedCursor {
            bytes: cursor_position_bytes(row, col),
            row,
            col,
        };
    }
    ProjectedCursor {
        bytes: Vec::new(),
        row: u16::MAX,
        col: u16::MAX,
    }
}

fn append_projected_snapshot_span(
    frame: &mut Vec<u8>,
    snapshot: &PaneRenderSnapshot,
    span: &TerminalPaneSpan,
    line: Option<&Arc<RenderedPaneRow>>,
) {
    frame.extend_from_slice(cursor_position_bytes(span.y, span.x).as_slice());
    frame.extend_from_slice(b"\x1b[0m");
    let Some(line) = line else {
        append_pane_row_tail(frame, span.y, span.x, span.cols, snapshot.terminal_cols, 0);
        return;
    };
    let projected = crop_rendered_pane_row(
        line.bytes(),
        span.source_col,
        usize::from(span.cols),
        &snapshot.utf8,
    );
    frame.extend_from_slice(projected.bytes());
    append_pane_row_tail(
        frame,
        span.y,
        span.x,
        span.cols,
        snapshot.terminal_cols,
        projected.columns(),
    );
}

fn is_plain_ascii_render_line(line: &[u8]) -> bool {
    line.iter().all(|byte| matches!(*byte, b' '..=b'~' | b'\t'))
}

fn next_synthetic_line_revision(revisions: &[u64]) -> u64 {
    revisions
        .iter()
        .map(|revision| revision & LINE_REVISION_ID_MASK)
        .max()
        .unwrap_or_default()
        .saturating_add(1_u64 << LINE_REVISION_GENERATION_BITS)
        & LINE_REVISION_ID_MASK
}

fn capture_visible_pane_lines(
    inputs: &PaneRenderInputs,
    screen: &Screen,
    row_start: usize,
    rows: usize,
    cols: u16,
    utf8: &rmux_core::Utf8Config,
) -> Vec<RenderedPaneRow> {
    let palette_screen = super::pane_palette::apply(screen, &inputs.palette);
    let screen = palette_screen.as_ref();
    let render_options = pane_render_options();
    let default_style = &inputs.default_style;
    // The delta path sees the same tmux rule as the full redraw: a selection
    // styles the cells it covers, and every other cell keeps its own empty
    // verdict. Measuring the selection-styled screen keeps the highlight and
    // still clears the tails of the rows the selection never reached.
    let overlaid_screen = inputs
        .selection_style
        .as_ref()
        .filter(|_| screen.has_selected_cells())
        .map(|style| {
            let mut selected = screen.clone();
            selected.overlay_style_on_selected(style);
            selected
        });
    let rendered_lines = if overlaid_screen.is_some() {
        {
            let mut styled = screen.clone();
            if let Some(style) = default_style {
                styled.overlay_default_style(style);
            }
            if let Some(style) = &inputs.selection_style {
                styled.overlay_style_on_selected(style);
            }
            styled
        }
        .capture_transcript_lines_independent(
            rmux_core::ScreenCaptureRange::default(),
            render_options,
        )
        .into_iter()
        .skip(row_start)
        .take(rows)
        .collect::<Vec<_>>()
    } else {
        (0..rows)
            .filter_map(|row| {
                let row = row_start.saturating_add(row);
                if let Some(style) = default_style.as_ref() {
                    screen.render_visible_line_independent_with_default_style(
                        row,
                        render_options,
                        style,
                    )
                } else {
                    screen.render_visible_line_independent(row, render_options)
                }
            })
            .collect::<Vec<_>>()
    };

    let tail_starts = capture_pane_empty_tail_starts_from(
        overlaid_screen.as_ref().unwrap_or(screen),
        row_start,
        rows,
        cols,
    );
    rendered_lines
        .into_iter()
        .enumerate()
        .map(|(row, line)| {
            truncate_rendered_pane_row(
                &line,
                usize::from(tail_starts.get(row).copied().unwrap_or(0)),
                utf8,
            )
        })
        .collect()
}

fn pane_render_options() -> GridRenderOptions {
    GridRenderOptions {
        with_sequences: true,
        include_empty_cells: true,
        trim_spaces: false,
        ..GridRenderOptions::default()
    }
}

#[cfg(test)]
mod tests;
