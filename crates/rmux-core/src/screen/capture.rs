use crate::grid::{Grid, GridCapture, GridRenderOptions, GridStringState};
use crate::hyperlinks::Hyperlinks;
use crate::style::Style;
use crate::transcript::{resolve_screen_capture_range, ScreenCaptureRange};

use super::Screen;

impl Screen {
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub(crate) fn capture_grid(&self, join_wrapped: bool) -> GridCapture {
        self.grid.capture(join_wrapped)
    }

    /// Captures a tmux-style line range over the current grid contents.
    #[must_use]
    pub fn capture_transcript(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<u8> {
        capture_grid_bytes(&self.grid, &self.hyperlinks, range, options)
    }

    /// Captures tmux-style per-line format flags for the selected physical rows.
    #[must_use]
    pub fn capture_line_format_flags(&self, range: ScreenCaptureRange) -> Vec<u8> {
        capture_grid_line_format_flags(&self.grid, range)
    }

    /// Captures physical lines with each line rendered from a fresh ANSI state.
    ///
    /// This is intended for renderers that repaint individual terminal rows:
    /// a row must carry its own SGR state instead of depending on a previous
    /// captured row having been emitted first.
    #[must_use]
    pub fn capture_transcript_lines_independent(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<Vec<u8>> {
        capture_grid_rows_independent(&self.grid, &self.hyperlinks, range, options)
            .into_iter()
            .map(|(bytes, _wrapped)| bytes)
            .collect()
    }

    /// Captures physical rows from fresh ANSI states together with each row's
    /// soft-wrap flag.
    ///
    /// Recovery renderers use the flag to preserve scrollback reflow: a
    /// wrapped row is followed immediately by the next row, while a hard line
    /// break is represented by CRLF.
    #[must_use]
    pub fn capture_transcript_rows_independent(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Vec<(Vec<u8>, bool)> {
        capture_grid_rows_independent(&self.grid, &self.hyperlinks, range, options)
    }

    #[must_use]
    /// Returns the monotonic mutation revision for one visible row.
    pub fn visible_line_revision(&self, row: usize) -> Option<u64> {
        self.grid
            .visible_line(u32::try_from(row).ok()?)
            .map(|line| line.revision())
    }

    #[must_use]
    /// Renders one visible row from a fresh ANSI state.
    pub fn render_visible_line_independent(
        &self,
        row: usize,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        let absolute_y = self.grid.hsize().checked_add(row)?;
        let mut state = GridStringState::default();
        self.grid
            .render_absolute_line(absolute_y, options, &mut state, Some(&self.hyperlinks))
            .map(String::into_bytes)
    }

    #[must_use]
    /// Renders one visible row from a fresh ANSI state after applying pane
    /// default-style to default cells only.
    pub fn render_visible_line_independent_with_default_style(
        &self,
        row: usize,
        options: GridRenderOptions,
        style: &Style,
    ) -> Option<Vec<u8>> {
        let mut state = GridStringState::default();
        self.grid
            .render_visible_line_with_default_style(
                row,
                options,
                &mut state,
                Some(&self.hyperlinks),
                style,
            )
            .map(String::into_bytes)
    }

    /// Captures the saved pre-alternate-screen copy when alternate mode is active.
    #[must_use]
    pub fn capture_saved_transcript(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<u8>> {
        self.saved_grid
            .as_ref()
            .map(|saved| capture_grid_bytes(&saved.grid, &self.hyperlinks, range, options))
    }

    /// Captures saved pre-alternate-screen rows from independent ANSI states.
    #[must_use]
    pub fn capture_saved_transcript_lines_independent(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<Vec<u8>>> {
        self.saved_grid.as_ref().map(|saved| {
            capture_grid_rows_independent(&saved.grid, &self.hyperlinks, range, options)
                .into_iter()
                .map(|(bytes, _wrapped)| bytes)
                .collect()
        })
    }

    /// Captures saved pre-alternate-screen rows and their soft-wrap flags.
    #[must_use]
    pub fn capture_saved_transcript_rows_independent(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<Vec<(Vec<u8>, bool)>> {
        self.saved_grid.as_ref().map(|saved| {
            capture_grid_rows_independent(&saved.grid, &self.hyperlinks, range, options)
        })
    }

    /// Captures the complete saved main screen while alternate mode is active.
    ///
    /// The live grid retains main-screen history while the saved grid retains
    /// its visible rows, so recovery must join those two stores explicitly.
    #[must_use]
    pub fn capture_saved_recovery_rows_independent(
        &self,
        options: GridRenderOptions,
    ) -> Option<Vec<(Vec<u8>, bool)>> {
        let saved = self.saved_grid.as_ref()?;
        let mut rows = capture_grid_absolute_rows_independent(
            &self.grid,
            &self.hyperlinks,
            0..self.grid.hsize(),
            options,
        );
        let saved_total =
            saved.grid.hsize() + usize::try_from(saved.grid.sy()).unwrap_or(usize::MAX);
        rows.extend(capture_grid_absolute_rows_independent(
            &saved.grid,
            &self.hyperlinks,
            saved.grid.hsize()..saved_total,
            options,
        ));
        Some(rows)
    }

    /// Captures tmux-style per-line format flags from the saved alternate screen.
    #[must_use]
    pub fn capture_saved_line_format_flags(&self, range: ScreenCaptureRange) -> Option<Vec<u8>> {
        self.saved_grid
            .as_ref()
            .map(|saved| capture_grid_line_format_flags(&saved.grid, range))
    }
}

fn capture_grid_rows_independent(
    grid: &Grid,
    hyperlinks: &Hyperlinks,
    range: ScreenCaptureRange,
    options: GridRenderOptions,
) -> Vec<(Vec<u8>, bool)> {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return Vec::new();
    };

    capture_grid_absolute_rows_independent(grid, hyperlinks, range, options)
}

fn capture_grid_absolute_rows_independent(
    grid: &Grid,
    hyperlinks: &Hyperlinks,
    rows: impl IntoIterator<Item = usize>,
    options: GridRenderOptions,
) -> Vec<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    for absolute_y in rows {
        let mut state = GridStringState::default();
        let Some(line) =
            grid.render_absolute_line(absolute_y, options, &mut state, Some(hyperlinks))
        else {
            continue;
        };
        output.push((
            line.into_bytes(),
            grid.absolute_line_wrapped(absolute_y).unwrap_or(false),
        ));
    }
    output
}

fn capture_grid_bytes(
    grid: &Grid,
    hyperlinks: &Hyperlinks,
    range: ScreenCaptureRange,
    options: GridRenderOptions,
) -> Vec<u8> {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return Vec::new();
    };

    let line_count = range.end().saturating_sub(*range.start()).saturating_add(1);
    let mut output = Vec::with_capacity(capture_capacity_hint(
        line_count,
        usize::try_from(grid.sx()).unwrap_or(usize::MAX),
    ));
    let mut state = GridStringState::default();
    for absolute_y in range {
        if grid
            .append_rendered_absolute_line(
                absolute_y,
                options,
                &mut state,
                Some(hyperlinks),
                &mut output,
            )
            .is_none()
        {
            continue;
        };
        let wrapped = grid.absolute_line_wrapped(absolute_y).unwrap_or(false);
        if !options.join_wrapped || !wrapped {
            state.reset_to_default_line_style(options, Some(hyperlinks), &mut output);
            output.push(b'\n');
        }
    }
    output
}

fn capture_grid_line_format_flags(grid: &Grid, range: ScreenCaptureRange) -> Vec<u8> {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return Vec::new();
    };

    let mut flags = Vec::new();
    for absolute_y in range {
        if grid.absolute_line(absolute_y).is_none() {
            continue;
        }
        flags.push(if grid.absolute_line_wrapped(absolute_y).unwrap_or(false) {
            b'W'
        } else {
            b'-'
        });
    }
    flags
}

fn capture_capacity_hint(line_count: usize, line_width: usize) -> usize {
    line_count
        .saturating_mul(line_width.saturating_add(1))
        .min(64 * 1024 * 1024)
}
