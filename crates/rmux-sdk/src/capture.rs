//! Text and styled snapshot capture helpers.

use std::future::{Future, IntoFuture};
use std::pin::Pin;

use crate::{Locator, Pane, PaneCell, PaneSnapshot, Result};

/// Zero-based rectangular region inside a pane snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Top row.
    pub row: u16,
    /// Left column.
    pub col: u16,
    /// Region height in rows.
    pub rows: u16,
    /// Region width in columns.
    pub cols: u16,
}

impl Rect {
    /// Creates a zero-based terminal rectangle.
    #[must_use]
    pub const fn new(row: u16, col: u16, rows: u16, cols: u16) -> Self {
        Self {
            row,
            col,
            rows,
            cols,
        }
    }
}

/// Captured text region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedRegion {
    /// Region captured from the snapshot.
    pub rect: Rect,
    /// Plain rendered text for the region.
    pub text: String,
    /// Row-major cells for the region when style preservation was requested.
    pub styled_cells: Option<Vec<PaneCell>>,
    /// Snapshot revision captured.
    pub revision: u64,
}

/// Awaitable capture builder.
#[derive(Debug, Clone)]
#[must_use = "capture builders do nothing unless awaited"]
pub struct CaptureBuilder {
    source: CaptureSource,
    preserve_style: bool,
}

#[derive(Debug, Clone)]
enum CaptureSource {
    Pane { pane: Pane, rect: Option<Rect> },
    Locator(Locator),
}

impl CaptureBuilder {
    pub(crate) fn pane(pane: Pane, rect: Option<Rect>) -> Self {
        Self {
            source: CaptureSource::Pane { pane, rect },
            preserve_style: false,
        }
    }

    pub(crate) fn locator(locator: Locator) -> Self {
        Self {
            source: CaptureSource::Locator(locator),
            preserve_style: false,
        }
    }

    /// Preserves row-major cells and style attributes in the capture result.
    pub const fn preserve_style(mut self, preserve: bool) -> Self {
        self.preserve_style = preserve;
        self
    }

    async fn run(self) -> Result<CapturedRegion> {
        match self.source {
            CaptureSource::Pane { pane, rect } => {
                let snapshot = pane.snapshot().await?;
                let rect = rect.unwrap_or_else(|| full_rect(&snapshot));
                Ok(capture_from_snapshot(&snapshot, rect, self.preserve_style))
            }
            CaptureSource::Locator(locator) => {
                let (_locator, snapshot, item) = locator.resolve_strict_with_wait().await?;
                let rect = Rect::new(
                    item.text_match.start_row,
                    item.text_match.start_col,
                    item.text_match
                        .end_row
                        .saturating_sub(item.text_match.start_row)
                        .saturating_add(1),
                    item.text_match
                        .end_col
                        .saturating_sub(item.text_match.start_col),
                );
                Ok(capture_from_snapshot(&snapshot, rect, self.preserve_style))
            }
        }
    }
}

impl IntoFuture for CaptureBuilder {
    type Output = Result<CapturedRegion>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

impl Pane {
    /// Captures a rectangular region from this pane's visible snapshot.
    pub fn capture_region(&self, rect: Rect) -> CaptureBuilder {
        CaptureBuilder::pane(self.clone(), Some(rect))
    }

    /// Captures this pane's full visible snapshot as text or styled cells.
    pub fn screenshot(&self) -> CaptureBuilder {
        CaptureBuilder::pane(self.clone(), None)
    }
}

impl Locator {
    /// Returns the strict visible text match bounding box.
    pub async fn bounding_box(self) -> Result<Rect> {
        let (_locator, _snapshot, item) = self.resolve_strict_with_wait().await?;
        Ok(Rect::new(
            item.text_match.start_row,
            item.text_match.start_col,
            item.text_match
                .end_row
                .saturating_sub(item.text_match.start_row)
                .saturating_add(1),
            item.text_match
                .end_col
                .saturating_sub(item.text_match.start_col),
        ))
    }

    /// Captures the strict visible text match.
    pub fn capture(self) -> CaptureBuilder {
        CaptureBuilder::locator(self)
    }

    /// Alias for [`Self::capture`] for Playwright-style wording.
    pub fn screenshot(self) -> CaptureBuilder {
        self.capture()
    }
}

fn capture_from_snapshot(
    snapshot: &PaneSnapshot,
    rect: Rect,
    preserve_style: bool,
) -> CapturedRegion {
    let rect = clamp_rect(snapshot, rect);
    let text = capture_text(snapshot, rect);
    let styled_cells = preserve_style.then(|| capture_cells(snapshot, rect));
    CapturedRegion {
        rect,
        text,
        styled_cells,
        revision: snapshot.revision,
    }
}

fn full_rect(snapshot: &PaneSnapshot) -> Rect {
    Rect::new(0, 0, snapshot.rows, snapshot.cols)
}

fn clamp_rect(snapshot: &PaneSnapshot, rect: Rect) -> Rect {
    let row = rect.row.min(snapshot.rows);
    let col = rect.col.min(snapshot.cols);
    let rows = rect.rows.min(snapshot.rows.saturating_sub(row));
    let cols = rect.cols.min(snapshot.cols.saturating_sub(col));
    Rect::new(row, col, rows, cols)
}

fn capture_text(snapshot: &PaneSnapshot, rect: Rect) -> String {
    (0..rect.rows)
        .map(|offset| capture_row_text(snapshot, rect.row + offset, rect.col, rect.cols))
        .collect::<Vec<_>>()
        .join("\n")
}

fn capture_row_text(snapshot: &PaneSnapshot, row: u16, col: u16, cols: u16) -> String {
    let mut text = String::new();
    let end = col.saturating_add(cols).min(snapshot.cols);
    for current_col in col..end {
        let Some(cell) = snapshot.cell(row, current_col) else {
            continue;
        };
        if !cell.is_padding() {
            text.push_str(cell.text());
        }
    }
    text.trim_end_matches(' ').to_owned()
}

fn capture_cells(snapshot: &PaneSnapshot, rect: Rect) -> Vec<PaneCell> {
    let mut cells = Vec::new();
    let end_row = rect.row.saturating_add(rect.rows).min(snapshot.rows);
    let end_col = rect.col.saturating_add(rect.cols).min(snapshot.cols);
    for row in rect.row..end_row {
        for col in rect.col..end_col {
            if let Some(cell) = snapshot.cell(row, col) {
                cells.push(cell.clone());
            }
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::{capture_from_snapshot, Rect};
    use crate::{PaneCell, PaneCursor, PaneGlyph, PaneSnapshot};

    fn cell(text: &str) -> PaneCell {
        PaneCell::new(PaneGlyph::new(text, 1))
    }

    #[test]
    fn capture_region_clamps_out_of_bounds_rects() {
        let snapshot = PaneSnapshot::new(
            4,
            2,
            vec![
                cell("a"),
                cell("b"),
                cell("c"),
                cell("d"),
                cell("e"),
                cell("f"),
                cell("g"),
                cell("h"),
            ],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        let capture = capture_from_snapshot(&snapshot, Rect::new(1, 2, 10, 10), false);

        assert_eq!(capture.rect, Rect::new(1, 2, 1, 2));
        assert_eq!(capture.text, "gh");
        assert!(capture.styled_cells.is_none());
    }

    #[test]
    fn styled_capture_preserves_row_major_cells_inside_clamped_region() {
        let snapshot = PaneSnapshot::new(
            3,
            1,
            vec![cell("x"), cell("y"), cell("z")],
            PaneCursor::default(),
        )
        .expect("valid snapshot");

        let capture = capture_from_snapshot(&snapshot, Rect::new(0, 1, 1, 9), true);

        assert_eq!(capture.text, "yz");
        let cells = capture.styled_cells.expect("styled cells");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].text(), "y");
        assert_eq!(cells[1].text(), "z");
    }
}
