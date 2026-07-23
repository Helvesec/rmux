use super::{CopyModeState, CopyModeSummary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CopyModeLineNumberMode {
    Off,
    Default,
    Absolute,
    Relative,
    Hybrid,
}

impl CopyModeLineNumberMode {
    pub(crate) fn parse(option: Option<&str>) -> Self {
        match option.unwrap_or("off") {
            "default" => Self::Default,
            "absolute" => Self::Absolute,
            "relative" => Self::Relative,
            "hybrid" => Self::Hybrid,
            _ => Self::Off,
        }
    }

    pub(crate) const fn uses_absolute_positions(self) -> bool {
        matches!(self, Self::Absolute | Self::Relative | Self::Hybrid)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CopyModeLineNumberLayout {
    mode: CopyModeLineNumberMode,
    width: usize,
    history_size: usize,
    scroll_position: usize,
    cursor_row: usize,
}

impl CopyModeLineNumberLayout {
    pub(crate) fn resolve(
        option: Option<&str>,
        enabled: bool,
        history_size: usize,
        rows: u16,
        scroll_position: usize,
        cursor_row: usize,
    ) -> Option<Self> {
        let mode = CopyModeLineNumberMode::parse(option);
        if !enabled || mode == CopyModeLineNumberMode::Off {
            return None;
        }
        let largest_line = history_size
            .saturating_add(usize::from(rows))
            .saturating_add(1);
        let digits = decimal_digits(largest_line).max(3);
        Some(Self {
            mode,
            width: digits.saturating_add(1),
            history_size,
            scroll_position,
            cursor_row,
        })
    }

    pub(crate) const fn width(self) -> usize {
        self.width
    }

    pub(crate) fn physical_width(self, pane_cols: u16) -> u16 {
        u16::try_from(self.width).unwrap_or(u16::MAX).min(pane_cols)
    }

    pub(crate) fn content_width(self, pane_cols: u16) -> u16 {
        if self.width >= usize::from(pane_cols) {
            1
        } else {
            pane_cols.saturating_sub(self.width as u16)
        }
    }

    pub(crate) fn number(self, row: usize) -> usize {
        let absolute = self
            .history_size
            .saturating_sub(self.scroll_position)
            .saturating_add(row)
            .saturating_add(1);
        let relative = row.abs_diff(self.cursor_row);
        match self.mode {
            CopyModeLineNumberMode::Off => 0,
            CopyModeLineNumberMode::Default => row.abs_diff(self.scroll_position),
            CopyModeLineNumberMode::Absolute => absolute,
            CopyModeLineNumberMode::Relative => relative,
            CopyModeLineNumberMode::Hybrid if row == self.cursor_row => absolute,
            CopyModeLineNumberMode::Hybrid => relative,
        }
    }

    pub(crate) fn row_is_current(self, row: usize) -> bool {
        row == self.cursor_row
    }

    pub(crate) fn cursor_x(self, pane_cols: u16, content_x: u32) -> u16 {
        if pane_cols == 0 {
            return 0;
        }
        let content_width = self.content_width(pane_cols);
        if content_x >= u32::from(content_width) {
            return pane_cols.saturating_sub(1);
        }
        self.physical_width(pane_cols)
            .saturating_add(u16::try_from(content_x).unwrap_or(u16::MAX))
            .min(pane_cols.saturating_sub(1))
    }

    pub(crate) fn mouse_content_x(self, pane_cols: u16, physical_x: u16) -> u32 {
        let content_width = self.content_width(pane_cols);
        u32::from(
            physical_x
                .saturating_sub(self.physical_width(pane_cols))
                .min(content_width.saturating_sub(1)),
        )
    }
}

impl CopyModeState {
    pub(crate) fn set_line_numbers_enabled(&mut self, enabled: bool) {
        self.line_numbers_enabled = enabled && !self.view_mode;
    }
}

impl CopyModeSummary {
    pub(crate) fn position_for_line_number_option(&self, option: Option<&str>) -> (usize, usize) {
        let absolute = self.line_numbers_enabled
            && CopyModeLineNumberMode::parse(option).uses_absolute_positions();
        if absolute {
            (
                self.history_size
                    .saturating_sub(self.scroll_position)
                    .saturating_add(1),
                self.history_size
                    .saturating_add(usize::from(self.backing_rows)),
            )
        } else {
            (self.scroll_position, self.history_size)
        }
    }
}

fn decimal_digits(mut value: usize) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::CopyModeLineNumberLayout;

    fn layout(
        mode: &str,
        history: usize,
        rows: u16,
        scroll: usize,
        cursor: usize,
    ) -> CopyModeLineNumberLayout {
        CopyModeLineNumberLayout::resolve(Some(mode), true, history, rows, scroll, cursor)
            .expect("line-number layout")
    }

    #[test]
    fn width_matches_tmux_history_and_resize_formula() {
        assert_eq!(layout("default", 0, 8, 0, 0).width(), 4);
        assert_eq!(layout("default", 990, 8, 0, 0).width(), 4);
        assert_eq!(layout("default", 991, 8, 0, 0).width(), 5);
        assert_eq!(layout("default", 1005, 8, 0, 0).width(), 5);
    }

    #[test]
    fn modes_match_tmux_37b_numbering() {
        let default = layout("default", 23, 8, 3, 5);
        let absolute = layout("absolute", 23, 8, 3, 5);
        let relative = layout("relative", 23, 8, 3, 5);
        let hybrid = layout("hybrid", 23, 8, 3, 5);

        assert_eq!((default.number(0), default.number(5)), (3, 2));
        assert_eq!((absolute.number(0), absolute.number(5)), (21, 26));
        assert_eq!((relative.number(0), relative.number(5)), (5, 0));
        assert_eq!((hybrid.number(0), hybrid.number(5)), (5, 26));
    }

    #[test]
    fn cursor_and_mouse_coordinates_account_for_the_internal_gutter() {
        let layout = layout("absolute", 23, 8, 0, 0);

        assert_eq!(layout.content_width(20), 16);
        assert_eq!(layout.cursor_x(20, 0), 4);
        assert_eq!(layout.cursor_x(20, 16), 19);
        assert_eq!(layout.mouse_content_x(20, 0), 0);
        assert_eq!(layout.mouse_content_x(20, 3), 0);
        assert_eq!(layout.mouse_content_x(20, 4), 0);
        assert_eq!(layout.mouse_content_x(20, 19), 15);
    }

    #[test]
    fn disabled_and_off_modes_have_no_layout() {
        assert!(CopyModeLineNumberLayout::resolve(Some("absolute"), false, 0, 8, 0, 0).is_none());
        assert!(CopyModeLineNumberLayout::resolve(Some("off"), true, 0, 8, 0, 0).is_none());
    }

    #[test]
    fn position_formats_follow_tmux_absolute_mode_semantics() {
        let summary = super::CopyModeSummary {
            view_mode: false,
            line_numbers_enabled: true,
            show_position: true,
            history_size: 31,
            backing_rows: 10,
            scroll_position: 31,
            rectangle_toggle: false,
            cursor_x: 0,
            cursor_y: 0,
            selection_start: None,
            selection_end: None,
            selection_active: false,
            selection_present: false,
            selection_mode: None,
            search_present: false,
            search_timed_out: false,
            search_count: 0,
            search_count_partial: false,
            search_match: None,
            copy_cursor_word: String::new(),
            copy_cursor_line: String::new(),
            copy_cursor_hyperlink: String::new(),
            pane_search_string: String::new(),
            top_line_time: 0,
        };

        // Measured against tmux 3.7b: absolute-like modes expose the
        // top-relative position and include visible rows in the limit.
        assert_eq!(
            summary.position_for_line_number_option(Some("absolute")),
            (1, 41)
        );
        assert_eq!(
            summary.position_for_line_number_option(Some("relative")),
            (1, 41)
        );
        assert_eq!(
            summary.position_for_line_number_option(Some("hybrid")),
            (1, 41)
        );
        assert_eq!(
            summary.position_for_line_number_option(Some("default")),
            (31, 31)
        );

        let mut mouse_origin = summary;
        mouse_origin.line_numbers_enabled = false;
        assert_eq!(
            mouse_origin.position_for_line_number_option(Some("absolute")),
            (31, 31),
            "tmux disables absolute position semantics with a mouse-origin entry"
        );
    }
}
