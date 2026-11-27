use crate::grid::{Grid, GridCapture, GridRenderOptions, GridStringState};
use crate::hyperlinks::Hyperlinks;
use crate::input::ScreenWriter;
use crate::style::Style;
use crate::transcript::{resolve_screen_capture_range, ScreenCaptureRange};

use super::Screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureSurfaceBoundary {
    Continuous,
    AfterAbsoluteRow(usize),
}

impl CaptureSurfaceBoundary {
    fn is_after(self, absolute_y: usize) -> bool {
        matches!(self, Self::AfterAbsoluteRow(boundary) if absolute_y == boundary)
    }

    fn separates_after(self, absolute_y: usize, capture_end: usize) -> bool {
        self.is_after(absolute_y) && capture_end > absolute_y
    }
}

mod physical;
pub use physical::LineFormatCapture;

pub use crate::recovery_row::RecoveryRow;

/// Bounded row renderer used to construct transport-safe recovery frames.
///
/// The renderer owns a size-limited hyperlink table, so rendering one row
/// cannot clone an unbounded OSC 8 URI from terminal-controlled state.
pub struct RecoveryRowRenderer<'a> {
    screen: &'a Screen,
    saved_grid: Option<Grid>,
    hyperlinks: Hyperlinks,
    metadata_complete: bool,
}

impl RecoveryRowRenderer<'_> {
    /// Returns whether every retained hyperlink fitted the metadata budget.
    #[must_use]
    pub const fn metadata_complete(&self) -> bool {
        self.metadata_complete
    }

    /// Renders one row of the active grid from a fresh ANSI state.
    #[must_use]
    pub fn active_row(&self, absolute_y: usize, options: GridRenderOptions) -> Option<RecoveryRow> {
        render_grid_row_independent(&self.screen.grid, &self.hyperlinks, absolute_y, options)
    }

    /// Returns one reconstructed main-history row's soft-wrap flag.
    #[must_use]
    pub fn recovery_history_row_wrapped(&self, absolute_y: usize) -> Option<bool> {
        let grid = self.saved_grid.as_ref().unwrap_or(&self.screen.grid);
        (absolute_y < grid.hsize())
            .then(|| grid.absolute_line_wrapped(absolute_y))
            .flatten()
    }

    /// Renders one row of the saved pre-alternate viewport.
    #[must_use]
    pub fn saved_row(&self, absolute_y: usize, options: GridRenderOptions) -> Option<RecoveryRow> {
        let saved = self.saved_grid.as_ref()?;
        render_grid_row_independent(
            saved,
            &self.hyperlinks,
            saved.hsize().saturating_add(absolute_y),
            options,
        )
    }

    /// Returns the history rows that belong to the reconstructed main screen.
    #[must_use]
    pub fn recovery_history_size(&self) -> usize {
        self.saved_grid
            .as_ref()
            .map_or_else(|| self.screen.grid.hsize(), Grid::hsize)
    }

    /// Renders one history row from the reconstructed main screen.
    #[must_use]
    pub fn recovery_history_row(
        &self,
        absolute_y: usize,
        options: GridRenderOptions,
    ) -> Option<RecoveryRow> {
        let grid = self.saved_grid.as_ref().unwrap_or(&self.screen.grid);
        (absolute_y < grid.hsize())
            .then(|| render_grid_row_independent(grid, &self.hyperlinks, absolute_y, options))
            .flatten()
    }
}

impl Screen {
    /// Creates a recovery renderer with bounded terminal-controlled metadata.
    #[must_use]
    pub fn recovery_row_renderer(
        &self,
        max_hyperlink_entry_bytes: usize,
        max_hyperlink_total_bytes: usize,
    ) -> RecoveryRowRenderer<'_> {
        let (hyperlinks, metadata_complete) = self
            .hyperlinks
            .clone_bounded(max_hyperlink_entry_bytes, max_hyperlink_total_bytes);
        let saved_grid = self.recovery_saved_grid();
        RecoveryRowRenderer {
            screen: self,
            saved_grid,
            hyperlinks,
            metadata_complete,
        }
    }

    fn recovery_saved_grid(&self) -> Option<Grid> {
        let saved_rows = usize::try_from(self.saved_grid.as_ref()?.grid.sy()).unwrap_or(usize::MAX);
        let restore_cursor = self.alternate_saved_cursor().is_some();
        let mut restored = self.clone();
        restored
            .grid
            .set_hlimit(restored.grid.hsize().saturating_add(saved_rows));
        restored.alternate_off(crate::input::COLOUR_DEFAULT, restore_cursor);
        Some(restored.grid)
    }

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
        let boundary = self.capture_surface_boundary();
        capture_grid_bytes(&self.grid, &self.hyperlinks, range, options, boundary, None)
    }

    /// Captures the link identities of a line range, as tmux's `-H` does.
    ///
    /// The source walks the cells of every captured line in their physical
    /// order, keeps the identities it has already written in one list for the
    /// WHOLE capture, stops at as many links as the screen is wide, separates
    /// the new links of one line with a space, omits a line that carried no new
    /// link, and ends every emitted line with a line feed unless a joined wrap
    /// carries it on (`tmux-3.7b/cmd-capture-pane.c:110-145,247-302`). The
    /// identity is the stored one, so two openings of one address that the
    /// store keeps apart stay two links here.
    #[must_use]
    pub fn capture_hyperlinks(&self, range: ScreenCaptureRange, join_wrapped: bool) -> Vec<u8> {
        capture_grid_hyperlinks(&self.grid, &self.hyperlinks, range, join_wrapped)
    }

    /// The same capture over the saved pre-alternate-screen copy.
    #[must_use]
    pub fn capture_saved_hyperlinks(
        &self,
        range: ScreenCaptureRange,
        join_wrapped: bool,
    ) -> Option<Vec<u8>> {
        self.saved_grid.as_ref().map(|saved| {
            capture_grid_hyperlinks(&saved.grid, &self.hyperlinks, range, join_wrapped)
        })
    }

    fn capture_surface_boundary(&self) -> CaptureSurfaceBoundary {
        self.capture_boundary_after().map_or(
            CaptureSurfaceBoundary::Continuous,
            CaptureSurfaceBoundary::AfterAbsoluteRow,
        )
    }

    /// Captures the legacy single-byte wrap flags (`W` or `-`).
    /// Use [`Self::capture_line_format_labels`] for combined shell and wrap flags.
    #[must_use]
    pub fn capture_line_format_flags(&self, range: ScreenCaptureRange) -> Vec<u8> {
        legacy_wrap_flags(self.capture_line_format_labels(range))
    }

    /// Captures tmux-style per-line format flags for the selected physical rows.
    #[must_use]
    pub fn capture_line_format_labels(&self, range: ScreenCaptureRange) -> Vec<&'static str> {
        self.capture_line_format(range).into_labels()
    }

    /// Captures per-line labels and explicit soft-wrap join continuations.
    ///
    /// The active alternate-screen boundary has no wrap continuation or `W`
    /// label. The stored main-screen wrap remains intact for restoration.
    #[must_use]
    pub fn capture_line_format(&self, range: ScreenCaptureRange) -> LineFormatCapture {
        capture_grid_line_format(&self.grid, range, self.capture_surface_boundary())
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
        self.saved_grid.as_ref().map(|saved| {
            capture_grid_bytes(
                &saved.grid,
                &self.hyperlinks,
                range,
                options,
                CaptureSurfaceBoundary::Continuous,
                None,
            )
        })
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

    /// Captures legacy single-byte wrap flags from the saved screen.
    #[must_use]
    pub fn capture_saved_line_format_flags(&self, range: ScreenCaptureRange) -> Option<Vec<u8>> {
        self.capture_saved_line_format_labels(range)
            .map(legacy_wrap_flags)
    }

    /// Captures tmux-style per-line format flags from the saved alternate screen.
    #[must_use]
    pub fn capture_saved_line_format_labels(
        &self,
        range: ScreenCaptureRange,
    ) -> Option<Vec<&'static str>> {
        self.capture_saved_line_format(range)
            .map(LineFormatCapture::into_labels)
    }

    /// Captures saved-screen labels and join continuations.
    #[must_use]
    pub fn capture_saved_line_format(
        &self,
        range: ScreenCaptureRange,
    ) -> Option<LineFormatCapture> {
        self.saved_grid.as_ref().map(|saved| {
            capture_grid_line_format(&saved.grid, range, CaptureSurfaceBoundary::Continuous)
        })
    }
}

fn render_grid_row_independent(
    grid: &Grid,
    hyperlinks: &Hyperlinks,
    absolute_y: usize,
    options: GridRenderOptions,
) -> Option<RecoveryRow> {
    let mut state = GridStringState::default();
    let (line, span) =
        grid.render_absolute_line_measured(absolute_y, options, &mut state, Some(hyperlinks))?;
    Some(RecoveryRow {
        bytes: line.into_bytes(),
        wrapped: grid.absolute_line_wrapped(absolute_y).unwrap_or(false),
        span,
        written_columns: grid.absolute_line(absolute_y)?.recovery_written_columns(),
        trailing_reflow_gap: grid
            .absolute_line_trailing_reflow_gap(absolute_y)
            .unwrap_or(0),
    })
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
    boundary: CaptureSurfaceBoundary,
    mut physical: Option<&mut LineFormatCapture>,
) -> Vec<u8> {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return Vec::new();
    };
    let capture_end = *range.end();

    let line_count = range.end().saturating_sub(*range.start()).saturating_add(1);
    let mut output = Vec::with_capacity(capture_capacity_hint(
        line_count,
        usize::try_from(grid.sx()).unwrap_or(usize::MAX),
    ));
    let mut state = GridStringState::default();
    for absolute_y in range {
        let row_start = output.len();
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
        let separated =
            !options.join_wrapped || !wrapped || boundary.separates_after(absolute_y, capture_end);
        if separated {
            state.reset_to_default_line_style(options, Some(hyperlinks), &mut output);
        }
        if let Some(physical) = physical.as_deref_mut() {
            physical.row_ranges.push(row_start..output.len());
        }
        if separated || physical.is_some() {
            output.push(b'\n');
        }
    }
    output
}

fn capture_grid_hyperlinks(
    grid: &Grid,
    hyperlinks: &Hyperlinks,
    range: ScreenCaptureRange,
    join_wrapped: bool,
) -> Vec<u8> {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return Vec::new();
    };
    let width = usize::try_from(grid.sx()).unwrap_or(usize::MAX);
    let mut seen: Vec<u32> = Vec::new();
    let mut output = Vec::new();
    for absolute_y in range {
        let Some(line) = grid.absolute_line(absolute_y) else {
            continue;
        };
        let mut links = Vec::new();
        for cell in line.cells() {
            let link = cell.link();
            if link == 0 || seen.contains(&link) {
                continue;
            }
            let Some(entry) = hyperlinks.get(link) else {
                continue;
            };
            if seen.len() == width {
                break;
            }
            seen.push(link);
            if !links.is_empty() {
                links.push(b' ');
            }
            links.extend_from_slice(entry.uri.as_bytes());
        }
        if links.is_empty() {
            continue;
        }
        output.extend_from_slice(&links);
        let wrapped = grid.absolute_line_wrapped(absolute_y).unwrap_or(false);
        if !join_wrapped || !wrapped {
            output.push(b'\n');
        }
    }
    output
}

fn capture_grid_line_format(
    grid: &Grid,
    range: ScreenCaptureRange,
    boundary: CaptureSurfaceBoundary,
) -> LineFormatCapture {
    let total_lines = grid.hsize() + usize::try_from(grid.sy()).unwrap_or(usize::MAX);
    let Some(range) = resolve_screen_capture_range(range, grid.hsize(), total_lines) else {
        return LineFormatCapture::default();
    };
    let capture_end = *range.end();

    let mut row_numbers = Vec::new();
    let mut labels = Vec::new();
    let mut join_continuations = Vec::new();
    for absolute_y in range {
        let Some(line) = grid.absolute_line(absolute_y) else {
            continue;
        };
        use crate::grid::GridLineFlags;
        const LABELS: [&str; 8] = ["-", "W", "P", "PW", "O", "OW", "OP", "OPW"];
        // tmux clears this wrap on alternate entry. Mask it only in the capture
        // surface so restoring the main screen still preserves its soft wrap.
        let wrapped =
            line.flags().contains(GridLineFlags::WRAPPED) && !boundary.is_after(absolute_y);
        let index = usize::from(line.flags().contains(GridLineFlags::START_OUTPUT)) * 4
            + usize::from(line.flags().contains(GridLineFlags::START_PROMPT)) * 2
            + usize::from(wrapped);
        // i128 holds every usize and their signed difference on supported targets.
        row_numbers.push(absolute_y as i128 - grid.hsize() as i128);
        labels.push(LABELS[index]);
        join_continuations.push(
            line.flags().contains(GridLineFlags::WRAPPED)
                && !boundary.separates_after(absolute_y, capture_end),
        );
    }
    LineFormatCapture {
        row_numbers,
        labels,
        join_continuations,
        row_ranges: Vec::new(),
    }
}

fn capture_capacity_hint(line_count: usize, line_width: usize) -> usize {
    line_count
        .saturating_mul(line_width.saturating_add(1))
        .min(64 * 1024 * 1024)
}

fn legacy_wrap_flags(labels: Vec<&str>) -> Vec<u8> {
    labels
        .into_iter()
        .map(|label| if label.contains('W') { b'W' } else { b'-' })
        .collect()
}
