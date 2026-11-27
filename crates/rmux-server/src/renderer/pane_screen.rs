use std::borrow::Cow;

use rmux_core::input::{mode, COLOUR_DEFAULT};
use rmux_core::style::{Style, StyleCell};
use rmux_core::{formats::FormatContext, GridRenderOptions, OptionStore, Pane, Screen, Utf8Config};
use rmux_proto::OptionName;

use crate::copy_mode::{CopyModeOverlayRange, CopyModeRenderOverlays, CopyModeRenderSnapshot};
use crate::format_runtime::RuntimeFormatContext;

use super::copy_mode_line_numbers::CopyModeLineNumberRenderer;
use super::pane_row::{
    append_pane_row_tail, capture_pane_empty_tail_starts_from, crop_rendered_pane_row,
};
use super::projection::{
    ProjectedPaneScrollbarContext, TerminalPaneProjection, TerminalSceneProjection,
};
use super::{background_sgr_parameter, cursor_position_bytes, style_sgr_bytes};

pub(crate) fn render_pane_screen(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        screen,
        PaneScreenCursorRestore::Pane,
        None,
    )
}

pub(crate) fn render_pane_screen_preserving_prompt_cursor(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        screen,
        PaneScreenCursorRestore::Prompt,
        None,
    )
}

pub(crate) fn render_copy_mode_pane_screen(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    snapshot: &CopyModeRenderSnapshot,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        &snapshot.screen,
        PaneScreenCursorRestore::Pane,
        Some(snapshot),
    )
}

pub(crate) fn render_copy_mode_pane_screen_preserving_prompt_cursor(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    snapshot: &CopyModeRenderSnapshot,
) -> Vec<u8> {
    render_pane_screen_with_cursor_restore(
        session,
        options,
        pane,
        &snapshot.screen,
        PaneScreenCursorRestore::Prompt,
        Some(snapshot),
    )
}

#[derive(Clone, Copy)]
enum PaneScreenCursorRestore {
    Pane,
    Prompt,
}

fn render_pane_screen_with_cursor_restore(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
    cursor_restore: PaneScreenCursorRestore,
    copy_mode_snapshot: Option<&CopyModeRenderSnapshot>,
) -> Vec<u8> {
    let copy_mode_overlays = copy_mode_snapshot.map(|snapshot| &snapshot.overlays);
    let palette = super::pane_palette::resolve(session, options, pane);
    let palette_screen = super::pane_palette::apply(screen, &palette);
    let screen = palette_screen.as_ref();
    let (history_size, alternate_on, copy_mode_scroll_position) = copy_mode_snapshot.map_or(
        (screen.history_size(), screen.is_alternate(), None),
        |snapshot| {
            (
                snapshot.history_size,
                snapshot.alternate_on,
                Some(snapshot.scroll_position),
            )
        },
    );
    let projection = TerminalSceneProjection::resolve(session, options);
    let Some(pane_projection) = projection.pane_projection(
        session,
        options,
        pane,
        ProjectedPaneScrollbarContext {
            history_size,
            alternate_on,
            copy_mode_scroll_position,
        },
    ) else {
        return Vec::new();
    };
    if pane_projection.spans().is_empty() && pane_projection.scrollbar().is_none() {
        return Vec::new();
    }
    let line_numbers = copy_mode_snapshot
        .and_then(|snapshot| CopyModeLineNumberRenderer::resolve(session, options, pane, snapshot));
    let line_number_layout = line_numbers
        .as_ref()
        .map(CopyModeLineNumberRenderer::layout);
    let physical_line_number_width = line_number_layout.map_or(0, |layout| {
        layout.physical_width(pane_projection.source_width())
    });
    let rendered_content_width = pane_projection
        .source_width()
        .saturating_sub(physical_line_number_width);

    let default_style = pane_default_style(session, options, pane);
    let cell_overlays =
        CopyModeCellOverlays::resolve(session, options, pane, screen, copy_mode_overlays);
    // tmux styles each cell copy first (`screen_select_cell`, `screen.c:599`)
    // and only then asks whether it is empty (`tty-draw.c:246-261`), so the
    // predicate must see the copy-mode deviations but not the pane default
    // style, which tmux keeps in `defaults` and never writes into the grid.
    let overlaid_screen = cell_overlays.as_ref().map(|overlays| {
        let mut overlaid = screen.clone();
        overlays.apply(&mut overlaid);
        overlaid
    });
    let tail_starts = capture_pane_empty_tail_starts_from(
        overlaid_screen.as_ref().unwrap_or(screen),
        pane_projection.source_row_start(),
        pane_projection.row_count(),
        rendered_content_width,
    );
    let styled_screen = match default_style.as_ref() {
        None => overlaid_screen.map_or(Cow::Borrowed(screen), Cow::Owned),
        Some(style) => {
            let mut styled = screen.clone();
            styled.overlay_default_style(style);
            if let Some(overlays) = cell_overlays.as_ref() {
                overlays.apply(&mut styled);
            }
            Cow::Owned(styled)
        }
    };
    let utf8 = Utf8Config::from_options(options);
    let render_options = GridRenderOptions {
        with_sequences: true,
        include_empty_cells: true,
        trim_spaces: false,
        ..GridRenderOptions::default()
    };
    let rendered_lines = (0..pane_projection.row_count())
        .map(|line| {
            let row = pane_projection.source_row_start().saturating_add(line);
            if let Some(style) = default_style.as_ref() {
                styled_screen
                    .render_visible_line_independent_with_default_style(row, render_options, style)
                    .unwrap_or_default()
            } else {
                styled_screen
                    .render_visible_line_independent(row, render_options)
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>();
    let rendered_bytes = rendered_lines.iter().map(Vec::len).sum::<usize>();
    let mut frame = Vec::with_capacity(
        rendered_bytes
            .saturating_add(pane_projection.spans().len().saturating_mul(20))
            .saturating_add(32),
    );
    frame.extend_from_slice(b"\x1b[s\x1b[?25l\x1b[0m");
    for span in pane_projection.spans() {
        let source_line = rendered_lines
            .get(span.line_index)
            .map_or(&[][..], Vec::as_slice);
        frame.extend_from_slice(cursor_position_bytes(span.y, span.x).as_slice());
        frame.extend_from_slice(b"\x1b[0m");
        let painted_columns = append_projected_pane_span(
            &mut frame,
            span,
            source_line,
            tail_starts
                .get(span.line_index)
                .copied()
                .map(usize::from)
                .unwrap_or(0),
            PaneSpanRenderOptions {
                line_numbers: line_numbers.as_ref(),
                line_number_width: physical_line_number_width,
                source_width: pane_projection.source_width(),
                default_style: default_style.as_ref(),
                utf8: &utf8,
            },
        );
        append_pane_row_tail(
            &mut frame,
            span.y,
            span.x,
            span.cols,
            session.terminal_size().cols,
            painted_columns,
        );
        if paints_right_edge_marker(
            line_number_layout,
            screen,
            span,
            pane_projection.source_width(),
        ) {
            frame.extend_from_slice(
                cursor_position_bytes(span.y, span.x.saturating_add(span.cols.saturating_sub(1)))
                    .as_slice(),
            );
            frame.extend_from_slice(b"\x1b[0m$\x1b[0m");
        }
    }
    if let Some(scrollbar) = pane_projection.scrollbar() {
        frame.extend_from_slice(scrollbar.frame().as_slice());
    }
    frame.extend_from_slice(b"\x1b[0m\x1b[u");
    match cursor_restore {
        PaneScreenCursorRestore::Pane => frame.extend_from_slice(
            final_pane_cursor_state(screen, &pane_projection, line_number_layout).as_slice(),
        ),
        PaneScreenCursorRestore::Prompt => frame.extend_from_slice(b"\x1b[?25h"),
    }
    frame
}

fn final_pane_cursor_state(
    screen: &Screen,
    projection: &TerminalPaneProjection,
    line_numbers: Option<crate::copy_mode::CopyModeLineNumberLayout>,
) -> Vec<u8> {
    let (cursor_x, cursor_y) = screen.cursor_position();
    let cursor_x = line_numbers.map_or_else(
        || cursor_x.min(u32::from(projection.source_width().saturating_sub(1))) as u16,
        |layout| layout.cursor_x(projection.source_width(), cursor_x),
    );
    let Some((x, y)) = projection.visible_terminal_cursor(cursor_x, cursor_y) else {
        return b"\x1b[?25l".to_vec();
    };
    let mut frame = cursor_position_bytes(y, x);
    if screen.mode() & mode::MODE_CURSOR == 0 {
        frame.extend_from_slice(b"\x1b[?25l");
    } else {
        frame.extend_from_slice(b"\x1b[?25h");
    }
    frame
}

struct PaneSpanRenderOptions<'a> {
    line_numbers: Option<&'a CopyModeLineNumberRenderer>,
    line_number_width: u16,
    source_width: u16,
    default_style: Option<&'a Style>,
    utf8: &'a Utf8Config,
}

fn append_projected_pane_span(
    frame: &mut Vec<u8>,
    span: &super::projection::TerminalPaneSpan,
    source_line: &[u8],
    tail_start: usize,
    options: PaneSpanRenderOptions<'_>,
) -> u16 {
    let PaneSpanRenderOptions {
        line_numbers,
        line_number_width,
        source_width,
        default_style,
        utf8,
    } = options;
    let span_start = span.source_col;
    let span_end = span_start.saturating_add(usize::from(span.cols));
    let mut columns = 0_u16;
    if let Some(line_numbers) = line_numbers {
        let prefix_width = usize::from(line_number_width);
        let prefix_start = span_start.min(prefix_width);
        let prefix_end = span_end.min(prefix_width);
        if prefix_start < prefix_end {
            let prefix = line_numbers.prefix_row(span.source_row, source_width);
            let segment = crop_rendered_pane_row(
                prefix.bytes(),
                prefix_start,
                prefix_end - prefix_start,
                utf8,
            );
            frame.extend_from_slice(segment.bytes());
            columns = columns.saturating_add(segment.columns());
        }
    }

    let content_start = span_start.max(usize::from(line_number_width));
    if content_start < span_end {
        let source_col = content_start.saturating_sub(usize::from(line_number_width));
        let available = tail_start.saturating_sub(source_col);
        let width = span_end.saturating_sub(content_start).min(available);
        if width != 0 {
            let line = crop_rendered_pane_row(source_line, source_col, width, utf8);
            frame.extend_from_slice(line.bytes());
            columns = columns.saturating_add(line.columns());
        }
    }
    if columns == 0 {
        if let Some(style) = default_style {
            frame.extend_from_slice(default_tail_sgr_bytes(style).as_slice());
        }
    }
    columns
}

fn default_tail_sgr_bytes(style: &Style) -> Vec<u8> {
    if style.cell.attr == 0 && style.cell.fg == COLOUR_DEFAULT && style.cell.us == COLOUR_DEFAULT {
        let background = if style.cell.bg == COLOUR_DEFAULT {
            style.fill
        } else {
            style.cell.bg
        };
        if let Some(parameter) = background_sgr_parameter(background) {
            return format!("\x1b[{parameter}m").into_bytes();
        }
    }
    style_sgr_bytes(style, true)
}

fn paints_right_edge_marker(
    line_numbers: Option<crate::copy_mode::CopyModeLineNumberLayout>,
    screen: &Screen,
    span: &super::projection::TerminalPaneSpan,
    source_width: u16,
) -> bool {
    let Some(layout) = line_numbers else {
        return false;
    };
    let right_edge = usize::from(source_width.saturating_sub(1));
    span.source_col <= right_edge
        && span.source_col.saturating_add(usize::from(span.cols)) > right_edge
        && span.source_row == usize::try_from(screen.cursor_position().1).unwrap_or(usize::MAX)
        && screen.cursor_position().0 >= u32::from(layout.content_width(source_width))
}

/// Expanded `copy-mode-selection-style` for the pane, resolved through the
/// option default (`#{E:mode-style}`) and the runtime format context — the
/// same machinery as the copy-mode position indicator. Passing the raw
/// option value into the cell style parser silently dropped the default
/// template, leaving selections visually unhighlighted (issue #90).
pub(crate) fn pane_selection_overlay_style(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
) -> Option<String> {
    pane_cell_overlay_style(session, options, pane, OptionName::CopyModeSelectionStyle)
        .map(|style| rmux_core::style_tostring(&style))
}

pub(super) fn pane_cell_overlay_style(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    option: OptionName,
) -> Option<Style> {
    let value = options.resolve_for_pane(
        session.name(),
        session.active_window_index(),
        pane.index(),
        option,
    )?;
    let context = FormatContext::new()
        .with_managed_session(session)
        .with_window(session.active_window_index(), session.window(), true, false)
        .with_window_pane(session.window(), pane);
    let runtime = RuntimeFormatContext::new(context)
        .with_options(options)
        .with_session(session)
        .with_window(session.active_window_index(), session.window())
        .with_pane(pane);
    let style = super::apply_runtime_style_overlay(&Style::default(), Some(value), &runtime);
    let rendered = rmux_core::style_tostring(&style);
    (!rendered.is_empty() && rendered != "default").then_some(style)
}

/// Builds the screen tmux measures its empty-cell predicate against.
///
/// Returns `None` when no copy-mode overlay and no selection style would
/// change a cell, so the caller keeps measuring the borrowed screen instead
/// of paying for a clone.
#[cfg(test)]
pub(super) fn overlay_styled_tail_screen(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    screen: &Screen,
    overlays: Option<&CopyModeRenderOverlays>,
) -> Option<Screen> {
    let cell_overlays = CopyModeCellOverlays::resolve(session, options, pane, screen, overlays)?;
    let mut overlaid = screen.clone();
    cell_overlays.apply(&mut overlaid);
    Some(overlaid)
}

/// Pairs an overlay range with its resolved style, resolving the option only
/// when a range exists. `Option::zip` would expand the style template on every
/// render, including the frames that carry no mark and no current match.
fn styled_range(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    range: Option<CopyModeOverlayRange>,
    option: OptionName,
) -> Option<(CopyModeOverlayRange, Style)> {
    let range = range?;
    let style = pane_cell_overlay_style(session, options, pane, option)?;
    Some((range, style))
}

/// The per-cell copy-mode deviations, resolved once so the measured screen and
/// the rendered screen cannot disagree about which cells carry a highlight.
///
/// This mirrors tmux's `screen_select_cell` (`screen.c:599`): a deviation is
/// applied to the cell itself, never to the whole pane.
struct CopyModeCellOverlays<'a> {
    mark: Option<(CopyModeOverlayRange, Style)>,
    matches: Option<(&'a [CopyModeOverlayRange], Style)>,
    current_match: Option<(CopyModeOverlayRange, Style)>,
    selection: Option<String>,
}

impl<'a> CopyModeCellOverlays<'a> {
    fn resolve(
        session: rmux_core::ManagedSessionView<'_>,
        options: &OptionStore,
        pane: &Pane,
        screen: &Screen,
        overlays: Option<&'a CopyModeRenderOverlays>,
    ) -> Option<Self> {
        let mark = styled_range(
            session,
            options,
            pane,
            overlays.and_then(|overlays| overlays.mark),
            OptionName::CopyModeMarkStyle,
        );
        let matches = overlays
            .filter(|overlays| !overlays.matches.is_empty())
            .and_then(|overlays| {
                pane_cell_overlay_style(session, options, pane, OptionName::CopyModeMatchStyle)
                    .map(|style| (overlays.matches.as_slice(), style))
            });
        let current_match = styled_range(
            session,
            options,
            pane,
            overlays.and_then(|overlays| overlays.current_match),
            OptionName::CopyModeCurrentMatchStyle,
        );
        let selection = screen
            .has_selected_cells()
            .then(|| pane_selection_overlay_style(session, options, pane))
            .flatten();
        (mark.is_some() || matches.is_some() || current_match.is_some() || selection.is_some())
            .then_some(Self {
                mark,
                matches,
                current_match,
                selection,
            })
    }

    /// Applies the deviations in the tmux 3.7b priority order: mark, then
    /// matches, then the current match, then the selection.
    fn apply(&self, screen: &mut Screen) {
        if let Some((range, style)) = self.mark.as_ref() {
            overlay_copy_mode_range(screen, *range, style);
        }
        if let Some((ranges, style)) = self.matches.as_ref() {
            for range in *ranges {
                overlay_copy_mode_range(screen, *range, style);
            }
        }
        if let Some((range, style)) = self.current_match.as_ref() {
            overlay_copy_mode_range(screen, *range, style);
        }
        if let Some(style) = self.selection.as_ref() {
            screen.overlay_style_on_selected(style);
        }
    }
}

fn overlay_copy_mode_range(screen: &mut Screen, range: CopyModeOverlayRange, style: &Style) {
    screen.overlay_style_on_row_range(range.row, range.start_x, range.end_x, style);
}

#[cfg(test)]
pub(crate) fn styled_pane_screen<'a>(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
    screen: &'a Screen,
) -> Cow<'a, Screen> {
    let default_style = pane_default_style(session, options, pane);
    let selection_style = screen
        .has_selected_cells()
        .then(|| pane_selection_overlay_style(session, options, pane))
        .flatten();
    if default_style.is_none() && selection_style.is_none() {
        return Cow::Borrowed(screen);
    }

    let mut styled_screen = screen.clone();
    if let Some(style) = default_style {
        styled_screen.overlay_default_style(&style);
    }
    if let Some(style) = selection_style {
        styled_screen.overlay_style_on_selected(&style);
    }
    Cow::Owned(styled_screen)
}

pub(crate) fn pane_default_style(
    session: rmux_core::ManagedSessionView<'_>,
    options: &OptionStore,
    pane: &Pane,
) -> Option<Style> {
    let mut style = Style::default();
    let base = StyleCell::default();
    let mut applied = false;
    for option in [OptionName::WindowStyle, OptionName::WindowActiveStyle] {
        if option == OptionName::WindowActiveStyle && pane.index() != session.active_pane_index() {
            continue;
        }
        let Some(value) = options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            option,
        ) else {
            continue;
        };
        if value.is_empty() || value == "default" {
            continue;
        }
        if style.parse_in_place(&base, value).is_ok() {
            applied = true;
        }
    }
    applied.then_some(style)
}
