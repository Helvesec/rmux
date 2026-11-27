use rmux_core::SignedPaneRect;

use crate::pane_scrollbar::{
    PaneScrollbar, PaneScrollbarConfig, PaneScrollbarLayout, ScrollbarPosition,
};

use super::super::pane_scrollbar::{RenderedPaneScrollbar, RenderedPaneScrollbarRow};
use super::{terminal_cell, visible_runs_for_row, ProjectedPaneScrollbarContext};

pub(super) fn projected_scrollbar(
    raw_source: SignedPaneRect,
    raw_clip: SignedPaneRect,
    local: PaneScrollbarLayout,
    context: ProjectedPaneScrollbarContext,
    config: &PaneScrollbarConfig,
    occluders: &[SignedPaneRect],
    y_offset: u16,
) -> Option<RenderedPaneScrollbar> {
    let scrollbar = PaneScrollbar::from_layout(
        local,
        context.history_size,
        context.alternate_on,
        config,
        context.copy_mode_scroll_position,
    )?;
    let content = local.content;
    let content_source = SignedPaneRect::new(
        raw_source.x().saturating_add(i32::from(content.x())),
        raw_source.y().saturating_add(i32::from(content.y())),
        i32::from(content.cols()),
        i32::from(content.rows()),
    )?;
    let (track_x, pad_x) = match scrollbar.position {
        ScrollbarPosition::Left => (
            raw_source.x(),
            raw_source.x().saturating_add(i32::from(local.width)),
        ),
        ScrollbarPosition::Right => {
            let pad_x = content_source.x().saturating_add(content_source.cols());
            (pad_x.saturating_add(i32::from(local.pad)), pad_x)
        }
    };
    let track = SignedPaneRect::new(
        track_x,
        content_source.y(),
        i32::from(local.width),
        content_source.rows(),
    )?;
    let pad = SignedPaneRect::new(
        pad_x,
        content_source.y(),
        i32::from(local.pad),
        content_source.rows(),
    )?;
    let rows = projected_scrollbar_rows(track, pad, raw_clip, occluders, y_offset);
    (!rows.is_empty()).then(|| RenderedPaneScrollbar::new(rows, scrollbar, config.style.clone()))
}

fn projected_scrollbar_rows(
    track: SignedPaneRect,
    pad: SignedPaneRect,
    raw_clip: SignedPaneRect,
    occluders: &[SignedPaneRect],
    y_offset: u16,
) -> Vec<RenderedPaneScrollbarRow> {
    let source_y = track.y().min(pad.y());
    let y_start = track.y().min(pad.y()).max(raw_clip.y());
    let y_end = track
        .y()
        .saturating_add(track.rows())
        .max(pad.y().saturating_add(pad.rows()))
        .min(raw_clip.y().saturating_add(raw_clip.rows()));
    (y_start..y_end)
        .filter_map(|y| {
            let pad = visible_runs_for_row(pad, raw_clip, y, occluders);
            let track = visible_runs_for_row(track, raw_clip, y, occluders);
            if pad.is_empty() && track.is_empty() {
                return None;
            }
            let (_, terminal_y) = terminal_cell(0, y, y_offset)?;
            Some(RenderedPaneScrollbarRow {
                y: terminal_y,
                source_row: u16::try_from(y.saturating_sub(source_y)).ok()?,
                pad,
                track,
            })
        })
        .collect()
}
