use super::{
    capture_grid_bytes, CaptureSurfaceBoundary, GridRenderOptions, Screen, ScreenCaptureRange,
};

/// Physical-row numbers, format labels and rows whose soft wrap may join the next
/// captured row. Formatted captures also retain exact rendered byte ranges,
/// independently of the renderer's `-J` cell and ANSI-state policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineFormatCapture {
    pub(super) row_numbers: Vec<i128>,
    pub(super) labels: Vec<&'static str>,
    pub(super) join_continuations: Vec<bool>,
    pub(super) row_ranges: Vec<std::ops::Range<usize>>,
}

impl LineFormatCapture {
    /// Returns each physical row's signed offset from the captured viewport.
    #[must_use]
    pub fn row_numbers(&self) -> &[i128] {
        &self.row_numbers
    }

    /// Returns tmux-style combined shell and wrap flags for each physical row.
    #[must_use]
    pub fn labels(&self) -> &[&'static str] {
        &self.labels
    }

    /// Returns whether each physical row may join its following captured row.
    #[must_use]
    pub fn join_continuations(&self) -> &[bool] {
        &self.join_continuations
    }

    /// Exact byte ranges of physical rows, excluding their capture separators.
    /// Empty for metadata-only captures and pending input.
    #[must_use]
    pub fn physical_row_ranges(&self) -> &[std::ops::Range<usize>] {
        &self.row_ranges
    }

    pub(super) fn into_labels(self) -> Vec<&'static str> {
        self.labels
    }
}

impl Screen {
    /// Renders with the requested cell policy while retaining physical rows for
    /// a later prefix/join pass. Byte ranges preserve widths without guessing
    /// from Unicode, tabs, ANSI sequences or compact ASCII storage.
    #[must_use]
    pub fn capture_transcript_with_line_format(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> (Vec<u8>, LineFormatCapture) {
        let mut physical = self.capture_line_format(range);
        let content = capture_grid_bytes(
            &self.grid,
            &self.hyperlinks,
            range,
            options,
            self.capture_surface_boundary(),
            Some(&mut physical),
        );
        (content, physical)
    }

    /// The same physical capture over the saved pre-alternate screen.
    #[must_use]
    pub fn capture_saved_transcript_with_line_format(
        &self,
        range: ScreenCaptureRange,
        options: GridRenderOptions,
    ) -> Option<(Vec<u8>, LineFormatCapture)> {
        let saved = self.saved_grid.as_ref()?;
        let mut physical = self.capture_saved_line_format(range)?;
        let content = capture_grid_bytes(
            &saved.grid,
            &self.hyperlinks,
            range,
            options,
            CaptureSurfaceBoundary::Continuous,
            Some(&mut physical),
        );
        Some((content, physical))
    }
}
