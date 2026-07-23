use rmux_core::style::Style;
use rmux_core::{OptionStore, Pane, Session};
use rmux_proto::OptionName;

use crate::copy_mode::{CopyModeLineNumberLayout, CopyModeRenderSnapshot, CopyModeSummary};

use super::pane_screen::pane_cell_overlay_style;
use super::style_sgr_bytes;

pub(super) struct CopyModeLineNumberRenderer {
    layout: CopyModeLineNumberLayout,
    normal_style: Style,
    current_style: Style,
}

impl CopyModeLineNumberRenderer {
    pub(super) fn resolve(
        session: &Session,
        options: &OptionStore,
        pane: &Pane,
        snapshot: &CopyModeRenderSnapshot,
    ) -> Option<Self> {
        let layout = layout_for_snapshot(session, options, pane, snapshot)?;
        let normal_style =
            pane_cell_overlay_style(session, options, pane, OptionName::CopyModeLineNumberStyle)
                .unwrap_or_default();
        let current_style = pane_cell_overlay_style(
            session,
            options,
            pane,
            OptionName::CopyModeCurrentLineNumberStyle,
        )
        .unwrap_or_default();
        Some(Self {
            layout,
            normal_style,
            current_style,
        })
    }

    pub(super) const fn layout(&self) -> CopyModeLineNumberLayout {
        self.layout
    }

    pub(super) fn append_prefix(&self, frame: &mut Vec<u8>, row: usize, pane_cols: u16) {
        let style = if self.layout.row_is_current(row) {
            &self.current_style
        } else {
            &self.normal_style
        };
        let field_width = self.layout.width().saturating_sub(1);
        let prefix = format!("{:>field_width$} ", self.layout.number(row));
        let visible_width = usize::from(self.layout.physical_width(pane_cols));

        frame.extend_from_slice(style_sgr_bytes(style, true).as_slice());
        frame.extend_from_slice(&prefix.as_bytes()[..visible_width.min(prefix.len())]);
        frame.extend_from_slice(b"\x1b[0m");
    }
}

pub(super) fn layout_for_snapshot(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    snapshot: &CopyModeRenderSnapshot,
) -> Option<CopyModeLineNumberLayout> {
    CopyModeLineNumberLayout::resolve(
        options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            OptionName::CopyModeLineNumbers,
        ),
        snapshot.line_numbers_enabled,
        snapshot.history_size,
        snapshot.screen.size().rows,
        snapshot.scroll_position,
        usize::try_from(snapshot.screen.cursor_position().1).unwrap_or(usize::MAX),
    )
}

pub(super) fn layout_for_summary(
    session: &Session,
    options: &OptionStore,
    pane: &Pane,
    summary: &CopyModeSummary,
) -> Option<CopyModeLineNumberLayout> {
    CopyModeLineNumberLayout::resolve(
        options.resolve_for_pane(
            session.name(),
            session.active_window_index(),
            pane.index(),
            OptionName::CopyModeLineNumbers,
        ),
        summary.line_numbers_enabled,
        summary.history_size,
        summary.backing_rows,
        summary.scroll_position,
        summary.cursor_y,
    )
}
