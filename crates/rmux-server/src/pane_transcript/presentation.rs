//! A single presentation selection shared by optimized and cloned captures.
use super::{PaneTranscript, PANE_SYNCHRONIZED_OUTPUT_TIMEOUT};
use rmux_core::{style::Style, GridRenderOptions, Screen};
use std::time::Instant;

/// The presentation state of a frame that was handed to a client: whether the
/// screen it was rendered from was the frozen DEC 2026 snapshot rather than the
/// live one.
///
/// A passthrough carrying an absolute cursor position must not be emitted
/// against a frozen frame, because the converged render that follows repaints —
/// and erases — the cells it drew into. `frozen` is produced alongside the selected screen by
/// [`PaneTranscript::read_presentation`]. The selection evaluates the fail-open
/// deadline once and retains that answer through rendering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FramePresentation {
    frozen: bool,
}

impl FramePresentation {
    /// A frame with no frozen presentation to converge: it was not rendered
    /// from a pane presentation screen at all — a copy-mode body, a recovery
    /// snapshot, a prompt overlay or a literal frame.
    #[cfg(feature = "web")]
    pub(crate) const fn converged() -> Self {
        Self { frozen: false }
    }

    pub(crate) const fn is_frozen(self) -> bool {
        self.frozen
    }

    /// Widens towards holding: a frame assembled from several reads is only
    /// converged when every one of them was.
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            frozen: self.frozen || other.frozen,
        }
    }
}

/// A pane presentation screen together with the state of the very read that
/// produced it. One lock acquisition answers both questions, so a frame
/// rendered from `screen` cannot be described by a later capture.
#[derive(Debug)]
pub(crate) struct PresentationCapture {
    screen: Screen,
    presentation: FramePresentation,
    plain_output_forwarding_safe: bool,
}

impl PresentationCapture {
    /// Wraps a screen that did not come from a presentation read — a recovery
    /// snapshot — so callers can treat every source uniformly.
    #[cfg(feature = "web")]
    pub(crate) const fn converged(screen: Screen) -> Self {
        Self {
            screen,
            presentation: FramePresentation::converged(),
            // A recovery screen has no parser state captured alongside it, so
            // it must establish a structured live snapshot before any output
            // can use the plain-byte fast path.
            plain_output_forwarding_safe: false,
        }
    }

    pub(crate) const fn screen(&self) -> &Screen {
        &self.screen
    }

    pub(crate) const fn presentation(&self) -> FramePresentation {
        self.presentation
    }

    pub(crate) const fn plain_output_forwarding_safe(&self) -> bool {
        self.plain_output_forwarding_safe
    }
}

pub(crate) struct PaneTranscriptRenderState {
    pub(crate) cursor_position: (u32, u32),
    pub(crate) cursor_style: u32,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) mode: u32,
    pub(crate) has_selected_cells: bool,
}

pub(crate) struct PaneVisibleLineCapture {
    pub(crate) revision: u64,
    pub(crate) rendered: Option<Vec<u8>>,
    pub(crate) tail_start: Option<u16>,
    pub(crate) previous_row: Option<usize>,
}

/// Borrowed selection; all frame data and provenance come from this screen.
pub(crate) struct PresentationRead<'a> {
    screen: &'a Screen,
    presentation: FramePresentation,
    plain_output_forwarding_safe: bool,
}

impl<'a> PresentationRead<'a> {
    pub(crate) const fn screen(&self) -> &'a Screen {
        self.screen
    }

    pub(crate) const fn presentation(&self) -> FramePresentation {
        self.presentation
    }

    pub(crate) const fn plain_output_forwarding_safe(&self) -> bool {
        self.plain_output_forwarding_safe
    }

    pub(crate) fn capture(&self) -> PresentationCapture {
        PresentationCapture {
            screen: self.screen.clone(),
            presentation: self.presentation,
            plain_output_forwarding_safe: self.plain_output_forwarding_safe,
        }
    }

    pub(crate) fn capture_projected_line_changes(
        &self,
        row_start: usize,
        rows: usize,
        options: GridRenderOptions,
        cols: u16,
        previous_revisions: Option<&[u64]>,
        default_style: Option<&Style>,
    ) -> Vec<PaneVisibleLineCapture> {
        let screen = self.screen;
        let revisions = (0..rows)
            .map(|row| {
                screen
                    .visible_line_revision(row_start.saturating_add(row))
                    .unwrap_or(0)
            })
            .collect::<Vec<_>>();
        let previous_rows = previous_revisions.and_then(|previous| {
            visible_revision_reuse_map(previous, &revisions).filter(|map| map.len() == rows)
        });

        (0..rows)
            .map(|row| {
                let previous_row = previous_rows
                    .as_ref()
                    .and_then(|rows| rows.get(row))
                    .and_then(|row| *row);
                let rendered = if previous_row.is_some() {
                    None
                } else {
                    render_visible_line(
                        screen,
                        row_start.saturating_add(row),
                        options,
                        default_style,
                    )
                };
                let tail_start = rendered.as_ref().and_then(|_| {
                    screen.visible_line_tmux_empty_tail_start(
                        row_start.saturating_add(row),
                        usize::from(cols),
                    )
                });
                PaneVisibleLineCapture {
                    revision: revisions[row],
                    rendered,
                    tail_start,
                    previous_row,
                }
            })
            .collect()
    }

    pub(crate) fn render_state(&self) -> PaneTranscriptRenderState {
        let screen = self.screen;
        PaneTranscriptRenderState {
            cursor_position: screen.cursor_position(),
            cursor_style: screen.cursor_style(),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode: screen.mode(),
            has_selected_cells: screen.has_selected_cells(),
        }
    }
}

impl PaneTranscript {
    /// Evaluate time once. Keeping the transcript locked does not stop time.
    pub(crate) fn read_presentation(&self) -> PresentationRead<'_> {
        let expired = self
            .synchronized_output_started_at
            .is_some_and(|started_at| {
                presentation_now().saturating_duration_since(started_at)
                    >= PANE_SYNCHRONIZED_OUTPUT_TIMEOUT
            });
        let live = self.terminal.screen();
        let screen = if expired {
            live
        } else {
            self.terminal.presentation_screen()
        };
        PresentationRead {
            screen,
            presentation: FramePresentation {
                frozen: !std::ptr::eq(screen, live),
            },
            plain_output_forwarding_safe: self.plain_output_forwarding_safe(),
        }
    }

    pub(crate) fn presentation_screen(&self) -> &Screen {
        self.read_presentation().screen()
    }

    pub(crate) fn capture_presentation_screen(&self) -> PresentationCapture {
        self.read_presentation().capture()
    }
}

fn render_visible_line(
    screen: &Screen,
    row: usize,
    options: GridRenderOptions,
    default_style: Option<&Style>,
) -> Option<Vec<u8>> {
    if let Some(style) = default_style {
        screen.render_visible_line_independent_with_default_style(row, options, style)
    } else {
        screen.render_visible_line_independent(row, options)
    }
}

pub(super) fn visible_revision_reuse_map(
    previous: &[u64],
    next: &[u64],
) -> Option<Vec<Option<usize>>> {
    if previous.len() != next.len() {
        return None;
    }
    let rows = next.len();
    let mut map = vec![None; rows];
    for (row, entry) in map.iter_mut().enumerate() {
        if previous[row] == next[row] {
            *entry = Some(row);
        }
    }

    if let Some(scroll_rows) =
        (1..rows).find(|scroll_rows| previous[*scroll_rows..] == next[..rows - *scroll_rows])
    {
        for (row, entry) in map.iter_mut().enumerate().take(rows - scroll_rows) {
            *entry = Some(row + scroll_rows);
        }
        return Some(map);
    }

    if let Some(scroll_rows) =
        (1..rows).find(|scroll_rows| previous[..rows - *scroll_rows] == next[*scroll_rows..])
    {
        for (row, entry) in map.iter_mut().enumerate().skip(scroll_rows) {
            *entry = Some(row - scroll_rows);
        }
    }

    Some(map)
}

fn presentation_now() -> Instant {
    #[cfg(test)]
    if let Some(now) = super::presentation_test_clock::now() {
        return now;
    }
    Instant::now()
}
