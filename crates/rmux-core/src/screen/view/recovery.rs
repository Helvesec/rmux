//! Bounded recovery clones of a screen: viewport, projection and metadata
//! shells that never copy unbounded scrollback, titles or hyperlinks.

use crate::grid::{Grid, GridLine};

use super::super::{SavedGrid, Screen};

impl Screen {
    /// Reports whether recovery metadata fits the supplied bounds without
    /// cloning viewport or scrollback rows.
    #[must_use]
    pub fn recovery_metadata_fits(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> bool {
        if self.title.len() > max_string_bytes
            || self.window_name.len() > max_string_bytes
            || self.path.len() > max_string_bytes
            || self
                .title_stack
                .iter()
                .any(|title| title.len() > max_string_bytes)
            || self
                .title_stack
                .iter()
                .try_fold(0_usize, |total, title| total.checked_add(title.len()))
                .is_none_or(|total| total > max_title_stack_bytes)
        {
            return false;
        }
        let (hyperlinks, complete) = self
            .hyperlinks
            .clone_bounded(max_hyperlink_entry_bytes, max_hyperlink_total_bytes);
        complete && (self.active_hyperlink == 0 || hyperlinks.get(self.active_hyperlink).is_some())
    }

    /// Clones only the visible viewport and bounded terminal metadata.
    ///
    /// Recovery callers validate geometry before invoking this method. Unlike
    /// [`Screen::clone`], this path never copies scrollback, saved alternate
    /// grids, passthrough payloads, or unbounded title/link strings.
    #[must_use]
    pub fn clone_recovery_viewport_bounded(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let (mut viewport, metadata_complete) = self.recovery_viewport_shell(
            max_string_bytes,
            max_title_stack_bytes,
            max_hyperlink_entry_bytes,
            max_hyperlink_total_bytes,
        );

        viewport.grid.replace_visible(self.grid.visible_lines());
        viewport.cursor_x = self.cursor_x.min(viewport.max_cursor_x());
        viewport.cursor_y = self.cursor_y.min(viewport.grid.sy().saturating_sub(1));
        viewport.pending_wrap = self.pending_wrap;
        viewport.saved_cursor_x = None;
        viewport.saved_cursor_y = None;
        viewport.saved_cursor_pending_wrap = false;
        viewport.rupper = self.rupper.min(viewport.grid.sy().saturating_sub(1));
        viewport.rlower = self.rlower.min(viewport.grid.sy().saturating_sub(1));
        viewport.tabs = self.tabs.clone();
        viewport.title_rename_enabled = self.title_rename_enabled;
        viewport.allow_rename = self.allow_rename;
        viewport.alternate_screen_enabled = self.alternate_screen_enabled;
        viewport.scroll_on_clear = self.scroll_on_clear;
        viewport.preserve_alternate_screen_cursor = self.preserve_alternate_screen_cursor;
        viewport.clear_combined_sequence_discards();

        (viewport, metadata_complete)
    }

    /// Clones a transport-bounded recovery projection without rendering it.
    ///
    /// The active viewport and saved alternate viewport are retained. Scrollback
    /// is limited to the newest complete logical-line suffix whose structural
    /// clone fits `max_history_bytes`.
    #[must_use]
    pub fn clone_recovery_projection_bounded(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
        max_history_bytes: usize,
    ) -> (Self, bool) {
        let (mut projection, metadata_complete) = self.recovery_viewport_shell(
            max_string_bytes,
            max_title_stack_bytes,
            max_hyperlink_entry_bytes,
            max_hyperlink_total_bytes,
        );

        projection.grid = self.grid.clone_recovery_projection(max_history_bytes);
        projection.cursor_x = self.cursor_x.min(projection.max_cursor_x());
        projection.cursor_y = self.cursor_y.min(projection.grid.sy().saturating_sub(1));
        projection.pending_wrap = self.pending_wrap;
        projection.saved_cursor_x = self.saved_cursor_x;
        projection.saved_cursor_y = self.saved_cursor_y;
        projection.saved_cursor_pending_wrap = self.saved_cursor_pending_wrap;
        projection.saved_grid = self.saved_grid.as_ref().map(|saved| SavedGrid {
            grid: saved.grid.clone_recovery_projection(0),
            history_enabled: saved.history_enabled,
        });
        projection.rupper = self.rupper.min(projection.grid.sy().saturating_sub(1));
        projection.rlower = self.rlower.min(projection.grid.sy().saturating_sub(1));
        projection.tabs = self.tabs.clone();
        projection.title_rename_enabled = self.title_rename_enabled;
        projection.allow_rename = self.allow_rename;
        projection.alternate_screen_enabled = self.alternate_screen_enabled;
        projection.scroll_on_clear = self.scroll_on_clear;
        projection.preserve_alternate_screen_cursor = self.preserve_alternate_screen_cursor;
        projection.clear_combined_sequence_discards();

        (projection, metadata_complete)
    }

    /// Clones a bounded viewport over absolute rows for Web scroll/copy-mode
    /// recovery without first cloning the backing history.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn clone_recovery_viewport_at_bounded(
        &self,
        top_line: usize,
        cursor_x: u32,
        cursor_absolute_y: usize,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let size = self.grid.size();
        let rows = usize::from(size.rows.max(1));
        let cols = u32::from(size.cols.max(1));
        let total_lines = self.absolute_line_count();
        let top_line = top_line.min(total_lines.saturating_sub(rows));
        let (mut viewport, metadata_complete) = self.recovery_viewport_shell(
            max_string_bytes,
            max_title_stack_bytes,
            max_hyperlink_entry_bytes,
            max_hyperlink_total_bytes,
        );
        viewport.projected_capture_boundary_after = self.projected_capture_boundary(top_line, rows);
        let lines = (0..rows)
            .map(|offset| {
                self.grid
                    .absolute_line(top_line + offset)
                    .cloned()
                    .unwrap_or_else(|| GridLine::new(cols))
            })
            .collect();
        viewport.grid.replace_visible(lines);
        viewport.cursor_x = cursor_x.min(viewport.max_cursor_x());
        viewport.cursor_y = if (top_line..top_line + rows).contains(&cursor_absolute_y) {
            (cursor_absolute_y - top_line) as u32
        } else {
            0
        };
        viewport.pending_wrap = false;
        viewport.rupper = 0;
        viewport.rlower = u32::from(size.rows.max(1)).saturating_sub(1);
        viewport.reset_tabs();
        viewport.clear_combined_sequence_discards();
        (viewport, metadata_complete)
    }

    #[allow(clippy::too_many_arguments)]
    fn recovery_viewport_shell(
        &self,
        max_string_bytes: usize,
        max_title_stack_bytes: usize,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> (Self, bool) {
        let mut viewport = Self::new(self.grid.size(), 0);
        let mut metadata_complete = true;
        viewport.mode = self.mode;
        viewport.cursor_style = self.cursor_style;
        viewport.cursor_default_style = self.cursor_default_style;
        viewport.title = bounded_utf8_string(&self.title, max_string_bytes, &mut metadata_complete);
        viewport.window_name =
            bounded_utf8_string(&self.window_name, max_string_bytes, &mut metadata_complete);
        viewport.path = bounded_utf8_string(&self.path, max_string_bytes, &mut metadata_complete);
        viewport.title_stack = bounded_title_stack(
            &self.title_stack,
            max_string_bytes,
            max_title_stack_bytes,
            &mut metadata_complete,
        );
        let (hyperlinks, hyperlinks_complete) = self
            .hyperlinks
            .clone_bounded(max_hyperlink_entry_bytes, max_hyperlink_total_bytes);
        metadata_complete &= hyperlinks_complete;
        viewport.active_hyperlink = if hyperlinks.get(self.active_hyperlink).is_some() {
            self.active_hyperlink
        } else {
            metadata_complete &= self.active_hyperlink == 0;
            0
        };
        viewport.hyperlinks = hyperlinks;
        viewport.metadata_revision = self.metadata_revision;
        viewport.utf8_config = self.utf8_config.clone();
        viewport.saved_cursor_x = None;
        viewport.saved_cursor_y = None;
        viewport.saved_cursor_pending_wrap = false;
        viewport.saved_grid = self.saved_grid.as_ref().map(|saved| SavedGrid {
            grid: Grid::new(self.grid.size(), 0),
            history_enabled: saved.history_enabled,
        });
        viewport.title_rename_enabled = self.title_rename_enabled;
        viewport.allow_rename = self.allow_rename;
        viewport.alternate_screen_enabled = self.alternate_screen_enabled;
        viewport.scroll_on_clear = self.scroll_on_clear;
        viewport.preserve_alternate_screen_cursor = self.preserve_alternate_screen_cursor;
        (viewport, metadata_complete)
    }
}

fn bounded_utf8_string(value: &str, max_bytes: usize, complete: &mut bool) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    *complete = false;
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn bounded_title_stack(
    titles: &[String],
    max_entry_bytes: usize,
    max_total_bytes: usize,
    complete: &mut bool,
) -> Vec<String> {
    let mut retained_reversed = Vec::new();
    let mut retained_bytes = 0_usize;
    for title in titles.iter().rev() {
        let title = bounded_utf8_string(title, max_entry_bytes, complete);
        let next = retained_bytes.saturating_add(title.len());
        if next > max_total_bytes {
            *complete = false;
            break;
        }
        retained_reversed.push(title);
        retained_bytes = next;
    }
    if retained_reversed.len() != titles.len() {
        *complete = false;
    }
    retained_reversed.reverse();
    retained_reversed
}
